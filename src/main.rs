mod api;
mod broker;
mod client;
mod utils;

use anyhow::Result;
use clap::Parser;
use tokio::signal;

use client::{AppConfig, ConsumedMessage, Consumer, MessageHandler, Producer, ProducerMessage};

#[derive(Parser, Debug)]
#[command(name = "rust-mq")]
#[command(about = "Rust Message Queue - Kafka-like message broker", long_about = None)]
struct Args {
    /// Running mode: broker, producer, or consumer
    #[arg(short, long, value_enum)]
    mode: Mode,

    /// Path to YAML configuration file
    #[arg(short, long)]
    config: Option<String>,

    /// Override broker address
    #[arg(long)]
    broker: Option<String>,
}

#[derive(Debug, Clone, clap::ValueEnum)]
enum Mode {
    Broker,
    Producer,
    Consumer,
}

fn init_logger(args: &Args) {
    let mut default_level = "info".to_string();
    if matches!(&args.mode, Mode::Broker) {
        if let Some(config_path) = &args.config {
            if let Ok(config) = broker::config::BrokerConfig::from_file(config_path) {
                default_level = config.log_level;
            }
        }
    }
    let _ =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(default_level))
            .try_init();
}

fn parse_host_port(addr: &str) -> (String, i32) {
    if let Some((host, port_str)) = addr.rsplit_once(':') {
        if let Ok(port) = port_str.parse::<i32>() {
            let host = if host == "0.0.0.0" {
                "localhost".to_string()
            } else {
                host.to_string()
            };
            return (host, port);
        }
    }
    ("localhost".to_string(), 50051)
}

/// Simple message handler that prints messages
struct PrintHandler;

#[async_trait::async_trait]
impl MessageHandler for PrintHandler {
    async fn handle(&self, message: ConsumedMessage) -> Result<()> {
        let value_str = message
            .value_as_string()
            .unwrap_or_else(|_| format!("<binary data: {} bytes>", message.value.len()));

        println!(
            "[{}:{}:{}] {}",
            message.topic, message.partition, message.offset, value_str
        );

        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    init_logger(&args);

    match args.mode {
        Mode::Broker => run_broker(args).await,
        Mode::Producer => run_producer(args).await,
        Mode::Consumer => run_consumer(args).await,
    }
}

async fn run_broker(args: Args) -> Result<()> {
    use broker::config::BrokerConfig;
    use broker::server::core::BrokerCore;
    use broker::server::kafka_server::KafkaBrokerServer;
    use broker::storage::traits::InMemoryStorage;
    use tokio::sync::mpsc;

    // Config file provided → always use KRaft (durable, works for both single-node and cluster).
    // No config file → in-memory single-node (fast dev/test mode).
    if let Some(config_path) = args.config {
        log::info!("Starting broker from config: {}", config_path);
        let config = BrokerConfig::from_file(&config_path)?;
        log::info!(
            "Starting KRaft broker node {} on {} (storage: {})",
            config.node_id,
            config.api_addr,
            config.storage_path
        );
        return run_kraft_cluster(config).await;
    }

    // No config file — in-memory single-node (dev mode).
    let config = BrokerConfig::default_single_node();
    log::info!("Starting in-memory single broker on {} (dev mode)", config.api_addr);

    let (rpc_tx, rpc_rx) = mpsc::channel(1000);
    let (broker_host, broker_port) = parse_host_port(&config.api_addr);
    let storage = InMemoryStorage::new_with_retention(
        config.node_id as i32,
        broker_host,
        broker_port,
        config.retention.clone(),
    );
    let broker_core = BrokerCore::new(rpc_rx, storage);
    tokio::spawn(async move {
        broker_core.run().await;
    });

    let grpc_server = KafkaBrokerServer::new(rpc_tx);
    let api_addr = config.api_addr.clone();
    tokio::spawn(async move {
        if let Err(e) = grpc_server.run(&api_addr).await {
            log::error!("Failed to start broker: {}", e);
        }
    });

    // Wait for shutdown signal
    signal::ctrl_c().await?;
    log::info!("Shutting down broker...");

    Ok(())
}

async fn run_kraft_cluster(config: broker::config::BrokerConfig) -> Result<()> {
    use broker::controller::raft_node::ControllerRaftNode;
    use broker::server::core::BrokerCore;
    use broker::server::kafka_server::KafkaBrokerServer;
    use broker::kraft::broker::{ControllerHandle, KRaftBroker};
    use broker::PeerInfo;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    // Build peer map.
    // - Single-node (no cluster config): empty — this node bootstraps alone.
    // - Joining an existing cluster (join_addr set): empty — leader replicates log after join.
    // - Multi-node bootstrap: seed from initial_members.
    let peers: std::collections::HashMap<u64, PeerInfo> =
        if config.join_addr.is_some() || config.cluster.is_none() {
            std::collections::HashMap::new()
        } else {
            config.cluster.as_ref().unwrap()
                .initial_members
                .iter()
                .map(|m| {
                    (
                        m.node_id,
                        PeerInfo {
                            rpc_addr: format!("http://{}", m.rpc_addr),
                            api_addr: m.api_addr.clone(),
                            sbe_tcp_addr: m.sbe_tcp_addr.clone(),
                        },
                    )
                })
                .collect()
        };

    let rebalance_timeout_ms = config
        .raft
        .as_ref()
        .map(|r| r.rebalance_timeout_ms)
        .unwrap_or(broker::config::RaftConfig::default().rebalance_timeout_ms);

    // Build ControllerRaftNode with the configured transport.
    let controller_storage = if config.transport == "sbe_tcp" {
        use broker::sbe_tcp::server::SbeTcpServer;
        use broker::sbe_tcp::transport::SbeTcpTransport;

        let peers_arc = Arc::new(std::sync::RwLock::new(peers));
        let (step_tx, step_rx) = tokio::sync::mpsc::unbounded_channel();
        let sbe_server = SbeTcpServer::new(step_tx.clone());
        let transport = SbeTcpTransport::new(config.node_id, peers_arc.clone());
        let (ctrl_node, storage) = ControllerRaftNode::new_with_transport(
            config.node_id,
            peers_arc,
            Some(config.storage_path.clone()),
            Box::new(transport),
            step_tx,
            step_rx,
            config.raft.clone(),
            config.api_addr.clone(),
        )
        .map_err(|e| anyhow::anyhow!("Failed to create KRaft controller: {:?}", e))?;
        tokio::spawn(ctrl_node.run());
        let rpc_addr = config.rpc_addr.clone();
        tokio::spawn(async move {
            if let Err(e) = sbe_server.serve(&rpc_addr).await {
                log::error!("SBE-TCP KRaft server error: {}", e);
            }
        });
        storage
    } else {
        // gRPC (default)
        use broker::grpc::{GrpcTransport, RaftGrpcServer, RaftNetworkSender};

        let peers_arc = Arc::new(std::sync::RwLock::new(peers));
        let (step_tx, step_rx) = tokio::sync::mpsc::unbounded_channel();
        let grpc_server = RaftGrpcServer::new(step_tx.clone());
        let transport = GrpcTransport(RaftNetworkSender::new(config.node_id, peers_arc.clone()));
        let (ctrl_node, storage) = ControllerRaftNode::new_with_transport(
            config.node_id,
            peers_arc,
            Some(config.storage_path.clone()),
            Box::new(transport),
            step_tx,
            step_rx,
            config.raft.clone(),
            config.api_addr.clone(),
        )
        .map_err(|e| anyhow::anyhow!("Failed to create KRaft controller: {:?}", e))?;
        tokio::spawn(ctrl_node.run());
        let rpc_addr = config.rpc_addr.clone();
        tokio::spawn(async move {
            if let Err(e) = grpc_server.serve(&rpc_addr).await {
                log::error!("gRPC KRaft server error: {}", e);
            }
        });
        storage
    };

    // Separate sled DB for partition data (distinct from the controller's Raft state).
    let data_path = format!("{}-kraft-data", config.storage_path);
    let sled_db = Arc::new(
        sled::open(&data_path)
            .map_err(|e| anyhow::anyhow!("Failed to open partition data store: {}", e))?,
    );

    let controller: Arc<dyn ControllerHandle> = Arc::new(controller_storage);
    let node_id = config.node_id;
    let broker = KRaftBroker::new(
        node_id,
        config.api_addr.clone(),
        controller.clone(),
        sled_db,
        rebalance_timeout_ms,
        1000, // isr_lag_max: follower may lag up to 1000 messages before leaving ISR
        config.default_replication_factor,
    )
    .await;

    // ISR tick loop: every 500 ms recompute ISR and propose any changes.
    let isr_mgr = broker.isr_mgr.clone();
    let ctrl_isr = controller.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
        loop {
            interval.tick().await;
            let changes = isr_mgr.tick().await;
            if changes.is_empty() {
                continue;
            }
            let meta = ctrl_isr.metadata().await;
            for change in changes {
                let replicas = meta
                    .partitions
                    .get(&(change.topic.clone(), change.partition))
                    .map(|p| p.replicas.clone())
                    .unwrap_or_default();
                if let Err(e) = ctrl_isr
                    .propose_partition_change(
                        change.topic,
                        change.partition,
                        node_id,
                        change.new_isr,
                        replicas,
                    )
                    .await
                {
                    log::warn!("ISR change propose failed: {}", e);
                }
            }
        }
    });

    // Heartbeat task: propose a timestamp every 3 s so the controller knows
    // this broker is alive.  NOT_LEADER is normal on non-leader nodes.
    {
        let ctrl_hb = controller.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(3));
            loop {
                interval.tick().await;
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                if let Err(e) = ctrl_hb.propose_broker_heartbeat(node_id, ts).await {
                    // NOT_LEADER is expected on follower nodes; only warn on other errors.
                    if !e.to_string().contains("NOT_LEADER") {
                        log::warn!("Broker heartbeat propose failed: {}", e);
                    } else {
                        log::debug!("Broker heartbeat skipped (not leader): {}", e);
                    }
                }
            }
        });
    }

    // Failure detector task: runs only on the active controller (leader).
    // Every 5 s scan brokers; declare dead if last_seen_ms > 0 and gap > 15 s.
    {
        use broker::controller::state_machine::compute_failover_assignments;
        let ctrl_fd = controller.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                interval.tick().await;
                if !ctrl_fd.is_controller() {
                    continue;
                }
                let meta = ctrl_fd.metadata().await;
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                for (broker_id, reg) in &meta.brokers {
                    // Grace period: skip brokers that have never sent a heartbeat.
                    if reg.last_seen_ms == 0 {
                        continue;
                    }
                    if now_ms.saturating_sub(reg.last_seen_ms) > 15_000 {
                        log::warn!(
                            "Broker {} appears dead (last_seen {}ms ago); electing new leaders",
                            broker_id,
                            now_ms - reg.last_seen_ms
                        );
                        let assignments = compute_failover_assignments(&meta, *broker_id);
                        if let Err(e) = ctrl_fd
                            .propose_mark_broker_dead(*broker_id, assignments)
                            .await
                        {
                            log::error!("propose_mark_broker_dead failed: {}", e);
                        }
                    }
                }
            }
        });
    }

    let (rpc_tx, rpc_rx) = mpsc::channel(1000);
    let broker_core = BrokerCore::new(rpc_rx, broker);
    tokio::spawn(async move {
        broker_core.run().await;
    });

    let grpc_server = KafkaBrokerServer::new(rpc_tx);
    let api_addr_str = config.api_addr.clone();
    tokio::spawn(async move {
        if let Err(e) = grpc_server.run(&api_addr_str).await {
            log::error!("Failed to start KRaft API server: {}", e);
        }
    });

    log::info!("KRaft broker {} started on {}", node_id, config.api_addr);

    // If join_addr is set, register this node with the existing cluster.
    // Follow NOT_LEADER redirects (error_code == 6) and retry on transient errors.
    if let Some(join_addr) = config.join_addr.clone() {
        use crate::api::broker::AddNodeRequest;
        use crate::client::kafka_broker_client::{KafkaBrokerClient, KafkaBrokerClientTrait};
        let api_addr = config.api_addr.clone();
        let rpc_addr = config.rpc_addr.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let mut target = join_addr.clone();
            let max_attempts = 10u32;
            for attempt in 1..=max_attempts {
                match KafkaBrokerClient::new(&target).await.map_err(|e| e.to_string()) {
                    Err(e) => {
                        log::warn!("Attempt {}/{}: cannot connect to {}: {}", attempt, max_attempts, target, e);
                    }
                    Ok(client) => {
                        let req = tonic::Request::new(AddNodeRequest {
                            node_id,
                            api_addr: api_addr.clone(),
                            rpc_addr: rpc_addr.clone(),
                        });
                        match client.add_node(req).await {
                            Ok(resp) if resp.error_code == 0 => {
                                log::info!("Successfully joined KRaft cluster via {}", target);
                                return;
                            }
                            Ok(resp) if resp.error_code == 6 && !resp.error_message.is_empty() => {
                                log::info!("Redirecting AddNode to controller at {}", resp.error_message);
                                target = resp.error_message.clone();
                                continue;
                            }
                            Ok(resp) => {
                                log::warn!("Attempt {}/{}: AddNode rejected (code {}): {}", attempt, max_attempts, resp.error_code, resp.error_message);
                            }
                            Err(e) => {
                                log::warn!("Attempt {}/{}: AddNode RPC failed: {}", attempt, max_attempts, e);
                            }
                        }
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            log::error!("Failed to join KRaft cluster after {} attempts", max_attempts);
        });
    }

    Ok(())
}

async fn run_producer(args: Args) -> Result<()> {
    // Load configuration
    let config = if let Some(config_path) = args.config {
        AppConfig::from_file(&config_path)?
    } else {
        log::warn!("No config file provided, using default configuration");
        AppConfig::default_producer("default-topic")
    };

    config.validate()?;

    let producer_config = config
        .producer
        .ok_or_else(|| anyhow::anyhow!("Producer configuration not found in config file"))?;

    let broker_addr = args.broker.as_ref().unwrap_or(&config.broker.address);

    log::info!("Starting producer for topic: {}", producer_config.topic);
    log::info!("Connecting to broker: {}", broker_addr);

    let producer = Producer::new(broker_addr, producer_config).await?;

    log::info!("Producer started. Waiting for input...");
    log::info!("Enter messages (one per line), Ctrl+C to exit:");

    // Setup shutdown handler
    let mut shutdown_signal = tokio::spawn(async {
        signal::ctrl_c().await.ok();
    });

    // Read from stdin and send messages
    use tokio::io::{AsyncBufReadExt, BufReader};
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut line = String::new();

    loop {
        tokio::select! {
            result = reader.read_line(&mut line) => {
                match result {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        let message = line.trim().to_string();
                        if !message.is_empty() {
                            let msg = ProducerMessage::new(message.as_bytes().to_vec());
                            if let Err(e) = producer.send(msg).await {
                                log::error!("Failed to send message: {}", e);
                            } else {
                                log::debug!("Message queued: {}", message);
                            }
                        }
                        line.clear();
                    }
                    Err(e) => {
                        log::error!("Failed to read from stdin: {}", e);
                        break;
                    }
                }
            }
            _ = &mut shutdown_signal => {
                log::info!("Shutdown signal received");
                break;
            }
        }
    }

    // Graceful shutdown
    producer.shutdown().await?;
    log::info!("Producer stopped");

    Ok(())
}

async fn run_consumer(args: Args) -> Result<()> {
    // Load configuration
    let config = if let Some(config_path) = args.config {
        AppConfig::from_file(&config_path)?
    } else {
        log::warn!("No config file provided, using default configuration");
        AppConfig::default_consumer("default-topic", Some("default-group".to_string()))
    };

    config.validate()?;

    let consumer_config = config
        .consumer
        .ok_or_else(|| anyhow::anyhow!("Consumer configuration not found in config file"))?;

    let broker_addr = args.broker.as_ref().unwrap_or(&config.broker.address);

    log::info!("Starting consumer for topic: {}", consumer_config.topic);
    log::info!("Connecting to broker: {}", broker_addr);

    let mut consumer = Consumer::new(broker_addr, consumer_config).await?;

    // Start consumer with print handler
    let handler = PrintHandler;
    consumer.start(handler).await?;

    log::info!("Consumer started. Press Ctrl+C to exit.");

    // Wait for shutdown signal
    signal::ctrl_c().await?;
    log::info!("Shutdown signal received");

    // Graceful shutdown
    consumer.shutdown().await?;
    log::info!("Consumer stopped");

    Ok(())
}
