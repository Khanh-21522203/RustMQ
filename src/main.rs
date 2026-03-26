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
    use broker::core::BrokerCore;
    use broker::kafka_broker_server::KafkaBrokerServer;
    use broker::multi_broker::MultiBroker;
    use broker::storage::InMemoryStorage;
    use tokio::sync::mpsc;

    if let Some(config_path) = args.config {
        log::info!("Starting broker from config: {}", config_path);
        let config = BrokerConfig::from_file(&config_path)?;
        if config.is_cluster_mode() {
            log::info!(
                "Starting multi-broker node {} on API: {}",
                config.node_id,
                config.api_addr
            );

            let cluster = config
                .cluster
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Cluster config missing"))?;

            // Build peer map: node_id -> PeerInfo for all cluster members
            use broker::raft_network::PeerInfo;
            let peers: std::collections::HashMap<u64, PeerInfo> = cluster
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
                .collect();

            let peer_count = peers.len();
            let transport_kind = config.transport.clone();
            let (multi_broker, raft_server) = MultiBroker::new(
                config.node_id,
                peers,
                Some(config.storage_path.clone()),
                Some(config.retention.clone()),
                config.raft.clone(),
                &transport_kind,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create multi-broker: {}", e))?;

            log::info!(
                "Multi-broker initialized with {} peers (transport={})",
                peer_count,
                transport_kind
            );

            let rpc_addr = config.rpc_addr.clone();
            tokio::spawn(async move {
                if let Err(e) = raft_server.serve(&rpc_addr).await {
                    log::error!("Raft transport server error: {}", e);
                }
            });

            let (rpc_tx, rpc_rx) = mpsc::channel(1000);
            let broker_core = BrokerCore::new(rpc_rx, multi_broker);
            tokio::spawn(async move {
                broker_core.run().await;
            });

            let grpc_server = KafkaBrokerServer::new(rpc_tx);
            let api_addr = config.api_addr.clone();
            tokio::spawn(async move {
                if let Err(e) = grpc_server.run(&api_addr).await {
                    log::error!("Failed to start Kafka API server: {}", e);
                }
            });

            log::info!("Raft broker started successfully on {}", config.api_addr);

            // If join_addr is set, register this node with the existing cluster leader.
            if let Some(join_addr) = config.join_addr.clone() {
                use crate::api::broker::AddNodeRequest;
                use crate::client::kafka_broker_client::{KafkaBrokerClient, KafkaBrokerClientTrait};
                let node_id = config.node_id;
                let api_addr = config.api_addr.clone();
                let rpc_addr = config.rpc_addr.clone();
                tokio::spawn(async move {
                    // Give the local gRPC server a moment to bind.
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    match KafkaBrokerClient::new(&join_addr).await.map_err(|e| e.to_string()) {
                        Ok(client) => {
                            let req = tonic::Request::new(AddNodeRequest {
                                node_id,
                                api_addr,
                                rpc_addr,
                            });
                            match client.add_node(req).await {
                                Ok(resp) if resp.error_code == 0 => {
                                    log::info!("Successfully joined cluster via {}", join_addr);
                                }
                                Ok(resp) => {
                                    log::error!(
                                        "Join failed (code {}): {}",
                                        resp.error_code,
                                        resp.error_message
                                    );
                                }
                                Err(e) => log::error!("AddNode RPC failed: {}", e),
                            }
                        }
                        Err(e) => log::error!("Could not connect to join_addr {}: {}", join_addr, e),

                    }
                });
            }
        } else if config.durable {
            // Single-node with sled-backed durable storage
            let (broker_host, broker_port) = parse_host_port(&config.api_addr);
            log::info!(
                "Starting durable single Kafka broker on {} (sled: {})",
                config.api_addr,
                config.storage_path
            );

            let (rpc_tx, rpc_rx) = mpsc::channel(1000);
            let storage = broker::sled_storage::SledStorage::open_with_retention(
                config.node_id as i32,
                broker_host,
                broker_port,
                &config.storage_path,
                config.retention.clone(),
            )?;
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

            log::info!("Durable single broker started successfully");
        } else {
            // Single-node with config file (in-memory)
            let (broker_host, broker_port) = parse_host_port(&config.api_addr);
            log::info!("Starting single Kafka broker on {}", config.api_addr);

            let (rpc_tx, rpc_rx) = mpsc::channel(1000);
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

            log::info!("Single broker started successfully");
        }
    } else {
        // Single-node with built-in defaults
        let config = BrokerConfig::default_single_node();
        log::info!("Starting single Kafka broker on {}", config.api_addr);

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

        log::info!("Single broker started successfully");
    }

    // Wait for shutdown signal
    signal::ctrl_c().await?;
    log::info!("Shutting down broker...");

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
