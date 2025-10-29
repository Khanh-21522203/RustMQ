# Rust-MQ Client Library

A reusable, production-ready Kafka client library for Rust with both programmatic API and CLI support.

## Features

- ✅ **Reusable**: Use as a library in your Rust projects
- ✅ **CLI Support**: Run as standalone producer/consumer/broker
- ✅ **YAML Configuration**: Easy configuration management
- ✅ **Graceful Shutdown**: Proper SIGINT/SIGTERM handling
- ✅ **Batch Processing**: Automatic message batching for producers
- ✅ **Auto-commit**: Automatic offset management for consumers
- ✅ **Async/Await**: Built on Tokio for high performance
- ✅ **SOLID Principles**: Clean, maintainable architecture
- ✅ **Type Safety**: Leverages Rust's type system

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│                      CLI Layer                          │
│  (main.rs - clap argument parsing, mode selection)      │
└────────────────────┬────────────────────────────────────┘
                     │
┌────────────────────▼────────────────────────────────────┐
│                Configuration Layer                       │
│     (config.rs - YAML parsing, validation)              │
└────────────────────┬────────────────────────────────────┘
                     │
┌────────────────────▼────────────────────────────────────┐
│                   Client Layer                          │
│  ┌──────────────┐          ┌──────────────┐            │
│  │   Producer   │          │   Consumer   │            │
│  │              │          │              │            │
│  │ - Batching   │          │ - Polling    │            │
│  │ - Async Send │          │ - Auto-commit│            │
│  └──────┬───────┘          └──────┬───────┘            │
│         │                         │                     │
│         └─────────┬───────────────┘                     │
│                   │                                     │
│         ┌─────────▼──────────┐                          │
│         │ KafkaBrokerClient  │                          │
│         │   (gRPC Client)    │                          │
│         └─────────┬──────────┘                          │
└───────────────────┼──────────────────────────────────────┘
                    │
┌───────────────────▼──────────────────────────────────────┐
│                Broker (Server)                           │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  │
│  │ gRPC Server  │→ │ Broker Core  │→ │   Storage    │  │
│  └──────────────┘  └──────────────┘  └──────────────┘  │
└──────────────────────────────────────────────────────────┘
```

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
# ... your other dependencies
```

Or use it as a standalone application by cloning this repository.

## Usage

### 1. CLI Usage

#### Start Broker

```bash
# Build the project
cargo build --release

# Run broker
cargo run -- --mode broker

# Or use the binary directly
./target/release/Rust-MQ --mode broker
```

#### Run Producer

```bash
# With config file
cargo run -- --mode producer --config config/producer.yaml

# Override broker address
cargo run -- --mode producer --config config/producer.yaml --broker http://192.168.1.100:50051

# Without config (uses defaults)
cargo run -- --mode producer
```

Type messages line by line, press Ctrl+C to exit.

#### Run Consumer

```bash
# With config file
cargo run -- --mode consumer --config config/consumer.yaml

# Override broker address
cargo run -- --mode consumer --config config/consumer.yaml --broker http://localhost:50051

# Without config (uses defaults)
cargo run -- --mode consumer
```

### 2. Programmatic Usage

#### Producer Example

```rust
use rust_mq::client::{Producer, ProducerConfig, ProducerMessage};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Create configuration
    let config = ProducerConfig {
        topic: "my-topic".to_string(),
        partition: 0,
        required_acks: 1,
        timeout_ms: 5000,
        batch_size: 100,
        flush_interval_ms: 100,
    };
    
    // Create producer
    let mut producer = Producer::new("http://localhost:50051", config).await?;
    
    // Start background task for auto-flushing
    producer.start().await?;
    
    // Send messages
    let msg = ProducerMessage::new(b"Hello, Kafka!");
    producer.send(msg).await?;
    
    // Send with partition override
    let msg = ProducerMessage::new(b"Hello again!").to_partition(1);
    producer.send(msg).await?;
    
    // Synchronous send (wait for result)
    let result = producer.send_sync(
        ProducerMessage::new(b"Important message")
    ).await?;
    
    println!("Message sent to partition {} at offset {}", 
             result.partition, result.offset);
    
    // Graceful shutdown
    producer.shutdown().await?;
    
    Ok(())
}
```

#### Consumer Example

```rust
use rust_mq::client::{Consumer, ConsumerConfig, ConsumedMessage, MessageHandler};
use anyhow::Result;

// Define a custom message handler
struct MyHandler;

#[async_trait::async_trait]
impl MessageHandler for MyHandler {
    async fn handle(&mut self, message: ConsumedMessage) -> Result<()> {
        let value = message.value_as_string()?;
        println!("Received: {}", value);
        
        // Your processing logic here
        // ...
        
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Create configuration
    let config = ConsumerConfig {
        topic: "my-topic".to_string(),
        partition: 0,
        group_id: Some("my-consumer-group".to_string()),
        offset: -2, // Start from earliest
        max_bytes: 1_048_576,
        max_wait_ms: 1000,
        min_bytes: 1,
        auto_commit: true,
        auto_commit_interval_ms: 5000,
        poll_interval_ms: 1000,
    };
    
    // Create consumer
    let mut consumer = Consumer::new("http://localhost:50051", config).await?;
    
    // Start consuming with handler
    let handler = MyHandler;
    consumer.start(handler).await?;
    
    // Wait for shutdown signal
    tokio::signal::ctrl_c().await?;
    
    // Graceful shutdown
    consumer.shutdown().await?;
    
    Ok(())
}
```

#### Manual Polling Example

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
    
    loop {
        // Poll for messages
        let messages = consumer.poll().await?;
        
        for message in messages {
            println!("Received: {:?}", message.value_as_string()?);
            
            // Process message...
        }
        
        // Manually commit offset
        consumer.commit().await?;
        
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
    }
    
    Ok(())
}
```

### 3. Configuration Files

#### Producer Configuration (`config/producer.yaml`)

```yaml
broker:
  address: "http://localhost:50051"
  timeout_secs: 30
  max_retries: 3

producer:
  topic: "events"
  partition: 0
  required_acks: 1
  timeout_ms: 5000
  batch_size: 100
  flush_interval_ms: 100
```

#### Consumer Configuration (`config/consumer.yaml`)

```yaml
broker:
  address: "http://localhost:50051"
  timeout_secs: 30
  max_retries: 3

consumer:
  topic: "events"
  partition: 0
  group_id: "my-consumer-group"
  offset: -2  # -2=earliest, -1=latest, 0+=specific
  max_bytes: 1048576
  max_wait_ms: 1000
  min_bytes: 1
  auto_commit: true
  auto_commit_interval_ms: 5000
  poll_interval_ms: 1000
```

## API Reference

### Producer

#### `Producer::new(broker_address, config) -> Result<Producer>`
Create a new producer instance.

#### `producer.start() -> Result<()>`
Start the producer with automatic batching and flushing.

#### `producer.send(message) -> Result<()>`
Send a message (adds to batch, async).

#### `producer.send_sync(message) -> Result<ProducerResult>`
Send a message and wait for result (synchronous).

#### `producer.flush() -> Result<()>`
Flush any pending messages immediately.

#### `producer.shutdown() -> Result<()>`
Gracefully shutdown the producer.

### Consumer

#### `Consumer::new(broker_address, config) -> Result<Consumer>`
Create a new consumer instance.

#### `consumer.start(handler) -> Result<()>`
Start consuming messages with a message handler.

#### `consumer.poll() -> Result<Vec<ConsumedMessage>>`
Poll for a single batch of messages (manual mode).

#### `consumer.commit() -> Result<()>`
Commit the current offset.

#### `consumer.current_offset() -> i64`
Get the current offset position.

#### `consumer.seek(offset)`
Seek to a specific offset.

#### `consumer.shutdown() -> Result<()>`
Gracefully shutdown the consumer.

### MessageHandler Trait

Implement this trait to create custom message handlers:

```rust
#[async_trait::async_trait]
pub trait MessageHandler: Send + Sync {
    async fn handle(&mut self, message: ConsumedMessage) -> Result<()>;
}
```

## Complete Examples

### Example: Simple Producer-Consumer

```bash
# Terminal 1: Start broker
cargo run -- --mode broker

# Terminal 2: Start consumer
cargo run -- --mode consumer --config config/consumer.yaml

# Terminal 3: Start producer and send messages
cargo run -- --mode producer --config config/producer.yaml
```

### Example: Programmatic Usage

See `examples/` directory for complete working examples:

```bash
# Run the example
cargo run --example producer_consumer
```

## Testing

```bash
# Run all tests
cargo test

# Run with logging
RUST_LOG=debug cargo test

# Run specific test
cargo test test_producer
```

## Environment Variables

- `RUST_LOG`: Set logging level (`error`, `warn`, `info`, `debug`, `trace`)
  ```bash
  RUST_LOG=debug cargo run -- --mode consumer
  ```

## Best Practices

1. **Always use graceful shutdown**: Call `shutdown()` before exiting
2. **Use consumer groups**: Enable offset tracking and fault tolerance
3. **Enable auto-commit**: For most use cases, auto-commit simplifies offset management
4. **Batch messages**: Use batching for better throughput
5. **Handle errors**: Always check and handle Result types properly
6. **Use structured logging**: Include context in log messages

## Troubleshooting

### Connection Issues

```rust
// Increase timeout
let mut config = BrokerConfig::default();
config.timeout_secs = 60;
```

### Message Loss

```rust
// Use all replicas acknowledgment
producer_config.required_acks = -1;
```

### Offset Management

```rust
// Start from earliest
consumer_config.offset = -2;

// Start from latest (skip existing)
consumer_config.offset = -1;

// Start from specific offset
consumer_config.offset = 1000;
```

## Performance Tips

1. **Increase batch size** for higher throughput:
   ```yaml
   producer:
     batch_size: 1000
     flush_interval_ms: 500
   ```

2. **Tune consumer fetch size**:
   ```yaml
   consumer:
     max_bytes: 10485760  # 10MB
   ```

3. **Use fire-and-forget for non-critical messages**:
   ```yaml
   producer:
     required_acks: 0
   ```

## Contributing

Contributions are welcome! Please ensure:
- Code follows Rust idioms
- Tests pass: `cargo test`
- Code is formatted: `cargo fmt`
- No clippy warnings: `cargo clippy`

## License

MIT License - See LICENSE file for details
