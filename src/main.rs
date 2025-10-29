mod api;
mod utils;
mod client;
mod broker;

use clap::Parser;
use anyhow::Result;
use tokio::signal;

use client::{AppConfig, Producer, Consumer, ProducerMessage, ConsumedMessage, MessageHandler};

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

/// Simple message handler that prints messages
struct PrintHandler;

#[async_trait::async_trait]
impl MessageHandler for PrintHandler {
    async fn handle(&mut self, message: ConsumedMessage) -> Result<()> {
        let value_str = message.value_as_string()
            .unwrap_or_else(|_| format!("<binary data: {} bytes>", message.value.len()));
        
        println!(
            "[{}:{}:{}] {}",
            message.topic,
            message.partition,
            message.offset,
            value_str
        );
        
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logger
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    
    let args = Args::parse();
    
    match args.mode {
        Mode::Broker => run_broker(args).await,
        Mode::Producer => run_producer(args).await,
        Mode::Consumer => run_consumer(args).await,
    }
}

async fn run_broker(_args: Args) -> Result<()> {
    use tokio::sync::mpsc;
    use broker::kafka_broker_server::KafkaBrokerServer;
    use broker::core::BrokerCore;
    use broker::storage::InMemoryStorage;
    
    let addr = "0.0.0.0:50051";
    log::info!("Starting Kafka broker on {}", addr);
    
    // Create channel for RPC communication
    let (rpc_tx, rpc_rx) = mpsc::channel(1000);
    
    // Create storage
    let storage = InMemoryStorage::new(1, "localhost".to_string(), 50051);
    
    // Create broker core
    let mut broker_core = BrokerCore::new(rpc_rx, storage);
    
    // Start broker core in background
    tokio::spawn(async move {
        broker_core.run().await;
    });
    
    // Create and start gRPC server
    let grpc_server = KafkaBrokerServer::new(rpc_tx);
    
    // Start server in background
    tokio::spawn(async move {
        if let Err(e) = grpc_server.run(addr).await {
            log::error!("Failed to start broker: {}", e);
        }
    });
    
    log::info!("Broker started successfully");
    
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
    
    let producer_config = config.producer.ok_or_else(|| {
        anyhow::anyhow!("Producer configuration not found in config file")
    })?;
    
    let broker_addr = args.broker.as_ref()
        .unwrap_or(&config.broker.address);
    
    log::info!("Starting producer for topic: {}", producer_config.topic);
    log::info!("Connecting to broker: {}", broker_addr);
    
    let mut producer = Producer::new(broker_addr, producer_config).await?;
    producer.start().await?;
    
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
    
    let consumer_config = config.consumer.ok_or_else(|| {
        anyhow::anyhow!("Consumer configuration not found in config file")
    })?;
    
    let broker_addr = args.broker.as_ref()
        .unwrap_or(&config.broker.address);
    
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
