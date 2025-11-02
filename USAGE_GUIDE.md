# Rust-MQ Usage Guide

Complete guide for using Rust-MQ as both a CLI tool and a library.

## Table of Contents

1. [Quick Start](#quick-start)
2. [CLI Usage](#cli-usage)
3. [Library Usage](#library-usage)
4. [Configuration](#configuration)
5. [Examples](#examples)
6. [Advanced Topics](#advanced-topics)

## Quick Start

### Build the Project

```bash
cargo build --release
```

### Run Complete Demo

```bash
# Terminal 1: Start broker
cargo run -- --mode broker

# Terminal 2: Start consumer
cargo run -- --mode consumer --config config/consumer.yaml

# Terminal 3: Send messages
cargo run -- --mode producer --config config/producer.yaml
# Type messages and press Enter
```

### Run Example

```bash
# Runs a complete producer-consumer example
cargo run --example producer_consumer
```

## CLI Usage

### Command-Line Arguments

```bash
rust-mq [OPTIONS]

Options:
  -m, --mode <MODE>        Running mode: broker, producer, or consumer
  -c, --config <CONFIG>    Path to YAML configuration file
      --broker <BROKER>    Override broker address
  -h, --help              Print help
```

### Mode: Broker

Start the Kafka broker server.

```bash
# Basic usage
cargo run -- --mode broker

# With logging
RUST_LOG=debug cargo run -- --mode broker
```

The broker will:
- Listen on `0.0.0.0:50051`
- Store messages in memory
- Handle producer and consumer requests
- Manage consumer groups and offsets

### Mode: Producer

Send messages to a topic.

```bash
# With config file
cargo run -- --mode producer --config config/producer.yaml

# Override broker address
cargo run -- --mode producer \
  --config config/producer.yaml \
  --broker http://192.168.1.100:50051

# Without config (uses defaults)
cargo run -- --mode producer
```

**Interactive Mode**: Type messages line by line, press Ctrl+C to exit.

```bash
$ cargo run -- --mode producer --config config/producer.yaml
[INFO] Starting producer for topic: events
[INFO] Connecting to broker: http://localhost:50051
[INFO] Producer started. Waiting for input...
[INFO] Enter messages (one per line), Ctrl+C to exit:
Hello, World!
This is message 2
Another message
^C
[INFO] Shutdown signal received
[INFO] Producer stopped
```

### Mode: Consumer

Receive messages from a topic.

```bash
# With config file
cargo run -- --mode consumer --config config/consumer.yaml

# Override broker address
cargo run -- --mode consumer \
  --config config/consumer.yaml \
  --broker http://localhost:50051

# Without config (uses defaults)
cargo run -- --mode consumer
```

**Output**: Messages are printed to stdout in the format:
```
[topic:partition:offset] message-content
```

Example:
```bash
$ cargo run -- --mode consumer --config config/consumer.yaml
[INFO] Starting consumer for topic: events
[INFO] Connecting to broker: http://localhost:50051
[INFO] Consumer started. Press Ctrl+C to exit.
[events:0:0] Hello, World!
[events:0:1] This is message 2
[events:0:2] Another message
^C
[INFO] Shutdown signal received
[INFO] Consumer stopped
```

## Library Usage

### Add as Dependency

If using as a library in another project:

```toml
[dependencies]
rust-mq = { path = "../Rust-MQ" }
# or
rust-mq = { git = "https://github.com/your/repo" }
```

### Producer API

#### Basic Producer

```rust
use rust_mq::client::{Producer, ProducerConfig, ProducerMessage};
use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    // Create configuration
    let config = ProducerConfig {
        topic: "my-topic".to_string(),
        partition: 0,
        required_acks: 1,
        timeout_ms: 5000,
        batch_size: 100,
        flush_interval_ms: 100,
    };
    
    // Create and start producer
    let mut producer = Producer::new("http://localhost:50051", config).await?;
    producer.start().await?;
    
    // Send messages
    for i in 0..10 {
        let msg = ProducerMessage::new(format!("Message {}", i));
        producer.send(msg).await?;
    }
    
    // Flush and shutdown
    producer.shutdown().await?;
    
    Ok(())
}
```

#### Advanced Producer Features

```rust
use rust_mq::client::{Producer, ProducerMessage};

// Send to specific partition
let msg = ProducerMessage::new(b"data").to_partition(1);
producer.send(msg).await?;

// Send with key
let msg = ProducerMessage::with_key(b"key123", b"value");
producer.send(msg).await?;

// Synchronous send (wait for result)
let result = producer.send_sync(
    ProducerMessage::new(b"important")
).await?;
println!("Sent to partition {} at offset {}", 
         result.partition, result.offset);

// Manual flush
producer.flush().await?;
```

### Consumer API

#### Basic Consumer with Handler

```rust
use rust_mq::client::{Consumer, ConsumerConfig, ConsumedMessage, MessageHandler};
use anyhow::Result;

// Define custom handler
struct MyHandler;

#[async_trait::async_trait]
impl MessageHandler for MyHandler {
    async fn handle(&mut self, message: ConsumedMessage) -> Result<()> {
        println!("Received: {:?}", message.value_as_string()?);
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Create configuration
    let config = ConsumerConfig {
        topic: "my-topic".to_string(),
        partition: 0,
        group_id: Some("my-group".to_string()),
        offset: -2, // Start from earliest
        max_bytes: 1_048_576,
        max_wait_ms: 1000,
        min_bytes: 1,
        auto_commit: true,
        auto_commit_interval_ms: 5000,
        poll_interval_ms: 1000,
    };
    
    // Create and start consumer
    let mut consumer = Consumer::new("http://localhost:50051", config).await?;
    consumer.start(MyHandler).await?;
    
    // Wait for shutdown signal
    tokio::signal::ctrl_c().await?;
    consumer.shutdown().await?;
    
    Ok(())
}
```

#### Manual Polling

```rust
use rust_mq::client::{Consumer, ConsumerConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = ConsumerConfig {
        topic: "my-topic".to_string(),
        partition: 0,
        group_id: Some("my-group".to_string()),
        offset: -2,
        max_bytes: 1_048_576,
        max_wait_ms: 1000,
        min_bytes: 1,
        auto_commit: false, // Manual commit
        auto_commit_interval_ms: 5000,
        poll_interval_ms: 1000,
    };
    
    let mut consumer = Consumer::new("http://localhost:50051", config).await?;
    
    for _ in 0..10 {
        // Poll for messages
        let messages = consumer.poll().await?;
        
        for message in messages {
            println!("Got: {:?}", message.value_as_string()?);
        }
        
        // Process and commit
        consumer.commit().await?;
    }
    
    Ok(())
}
```

#### Offset Management

```rust
// Seek to specific offset
consumer.seek(1000);

// Get current offset
let offset = consumer.current_offset();
println!("Current offset: {}", offset);

// Commit current position
consumer.commit().await?;
```

## Configuration

### Configuration File Structure

```yaml
broker:
  address: "http://localhost:50051"
  timeout_secs: 30
  max_retries: 3

producer:  # Optional
  topic: "my-topic"
  partition: 0
  required_acks: 1
  timeout_ms: 5000
  batch_size: 100
  flush_interval_ms: 100

consumer:  # Optional
  topic: "my-topic"
  partition: 0
  group_id: "my-group"
  offset: -2
  max_bytes: 1048576
  max_wait_ms: 1000
  min_bytes: 1
  auto_commit: true
  auto_commit_interval_ms: 5000
  poll_interval_ms: 1000
```

### Load Configuration Programmatically

```rust
use rust_mq::client::AppConfig;

// From file
let config = AppConfig::from_file("config/producer.yaml")?;

// From YAML string
let yaml = r#"
broker:
  address: "http://localhost:50051"
producer:
  topic: "test"
"#;
let config = AppConfig::from_yaml_str(yaml)?;

// Default configs
let config = AppConfig::default_producer("my-topic");
let config = AppConfig::default_consumer("my-topic", Some("group-id".to_string()));

// Validate
config.validate()?;
```

### Configuration Options Explained

#### Broker Config
- `address`: Broker URL (default: `http://localhost:50051`)
- `timeout_secs`: Connection timeout (default: 30)
- `max_retries`: Retry attempts (default: 3)

#### Producer Config
- `topic`: Target topic name
- `partition`: Default partition (default: 0)
- `required_acks`: 
  - `-1` = All replicas (most reliable)
  - `0` = No acknowledgment (fastest)
  - `1` = Leader only (balanced)
- `timeout_ms`: Request timeout (default: 5000)
- `batch_size`: Messages per batch (default: 100)
- `flush_interval_ms`: Auto-flush interval (default: 100)

#### Consumer Config
- `topic`: Source topic name
- `partition`: Partition to consume (default: 0)
- `group_id`: Consumer group for offset tracking
- `offset`: Starting position:
  - `-2` = Earliest (from beginning)
  - `-1` = Latest (skip existing)
  - `0+` = Specific offset
- `max_bytes`: Max fetch size (default: 1MB)
- `max_wait_ms`: Max wait for messages (default: 1000)
- `min_bytes`: Min bytes before returning (default: 1)
- `auto_commit`: Enable automatic offset commits
- `auto_commit_interval_ms`: Commit frequency (default: 5000)
- `poll_interval_ms`: Polling frequency (default: 1000)

## Examples

### Example 1: Log Processing System

```rust
use rust_mq::client::*;
use anyhow::Result;

struct LogProcessor;

#[async_trait::async_trait]
impl MessageHandler for LogProcessor {
    async fn handle(&mut self, message: ConsumedMessage) -> Result<()> {
        let log_entry = message.value_as_string()?;
        
        if log_entry.contains("ERROR") {
            eprintln!("🔴 ERROR: {}", log_entry);
            // Send alert, save to error log, etc.
        } else if log_entry.contains("WARN") {
            println!("⚠️  WARN: {}", log_entry);
        } else {
            println!("✓ {}", log_entry);
        }
        
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = ConsumerConfig {
        topic: "application-logs".to_string(),
        partition: 0,
        group_id: Some("log-processor".to_string()),
        offset: -1, // Start from latest
        auto_commit: true,
        ..Default::default()
    };
    
    let mut consumer = Consumer::new("http://localhost:50051", config).await?;
    consumer.start(LogProcessor).await?;
    
    tokio::signal::ctrl_c().await?;
    consumer.shutdown().await?;
    
    Ok(())
}
```

### Example 2: Event-Driven Microservice

```rust
use rust_mq::client::*;
use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct OrderEvent {
    order_id: String,
    customer_id: String,
    amount: f64,
}

struct OrderProcessor;

#[async_trait::async_trait]
impl MessageHandler for OrderProcessor {
    async fn handle(&mut self, message: ConsumedMessage) -> Result<()> {
        let event: OrderEvent = serde_json::from_slice(&message.value)?;
        
        println!("Processing order: {}", event.order_id);
        
        // Process order
        process_payment(&event).await?;
        update_inventory(&event).await?;
        send_confirmation(&event).await?;
        
        Ok(())
    }
}

async fn process_payment(event: &OrderEvent) -> Result<()> {
    // Payment processing logic
    Ok(())
}

async fn update_inventory(event: &OrderEvent) -> Result<()> {
    // Inventory update logic
    Ok(())
}

async fn send_confirmation(event: &OrderEvent) -> Result<()> {
    // Send confirmation email
    Ok(())
}
```

### Example 3: Data Pipeline

```rust
// Producer: Ingest data
let mut producer = Producer::new("http://localhost:50051", producer_config).await?;
producer.start().await?;

// Read from data source and publish
for record in data_source {
    let json = serde_json::to_vec(&record)?;
    producer.send(ProducerMessage::new(json)).await?;
}

producer.shutdown().await?;
```

```rust
// Consumer: Transform and load
struct ETLProcessor {
    database: DatabaseConnection,
}

#[async_trait::async_trait]
impl MessageHandler for ETLProcessor {
    async fn handle(&mut self, message: ConsumedMessage) -> Result<()> {
        // Extract
        let data: RawData = serde_json::from_slice(&message.value)?;
        
        // Transform
        let transformed = transform_data(data)?;
        
        // Load
        self.database.insert(transformed).await?;
        
        Ok(())
    }
}
```

## Advanced Topics

### Graceful Shutdown

Always shutdown gracefully to ensure no message loss:

```rust
// Producer
producer.shutdown().await?;  // Flushes pending messages

// Consumer
consumer.shutdown().await?;  // Commits final offset
```

### Error Handling

```rust
#[async_trait::async_trait]
impl MessageHandler for ResilientHandler {
    async fn handle(&mut self, message: ConsumedMessage) -> Result<()> {
        match process_message(&message).await {
            Ok(_) => Ok(()),
            Err(e) => {
                log::error!("Failed to process message: {}", e);
                // Send to dead letter queue
                send_to_dlq(&message).await?;
                Ok(()) // Don't fail the consumer
            }
        }
    }
}
```

### Performance Tuning

```rust
// High throughput producer
let producer_config = ProducerConfig {
    batch_size: 1000,           // Large batches
    flush_interval_ms: 500,      // Less frequent flushes
    required_acks: 0,            // Fire and forget
    ..config
};

// High throughput consumer
let consumer_config = ConsumerConfig {
    max_bytes: 10_485_760,       // 10MB per fetch
    poll_interval_ms: 100,       // Fast polling
    auto_commit_interval_ms: 10000, // Less frequent commits
    ..config
};
```

### Logging

```bash
# Set log level
RUST_LOG=debug cargo run -- --mode consumer

# Filter by module
RUST_LOG=rust_mq::client=debug cargo run -- --mode producer

# Multiple levels
RUST_LOG=info,rust_mq::client::producer=debug cargo run
```

## Troubleshooting

### Can't Connect to Broker

```bash
# Check if broker is running
cargo run -- --mode broker

# Verify address in config
broker:
  address: "http://localhost:50051"
```

### Messages Not Appearing

```rust
// Producer: Ensure flush
producer.flush().await?;

// Consumer: Check starting offset
consumer_config.offset = -2; // Start from earliest
```

### Offset Not Committing

```rust
// Enable auto-commit
consumer_config.auto_commit = true;

// Or manual commit
consumer.commit().await?;
```

## See Also

- [CLIENT_LIBRARY.md](CLIENT_LIBRARY.md) - Complete API reference
- [BROKER_IMPLEMENTATION.md](BROKER_IMPLEMENTATION.md) - Broker architecture
- [examples/](examples/) - More code examples
