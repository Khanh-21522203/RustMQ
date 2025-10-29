# Quick Reference Card

## Build & Run

```bash
# Build
cargo build --release

# Run broker
cargo run -- --mode broker

# Run producer
cargo run -- --mode producer --config config/producer.yaml

# Run consumer  
cargo run -- --mode consumer --config config/consumer.yaml

# Run example
cargo run --example producer_consumer
```

## CLI Commands

```bash
# Broker
rust-mq --mode broker

# Producer (interactive)
rust-mq --mode producer --config <file>

# Consumer (continuous)
rust-mq --mode consumer --config <file>

# Override broker
rust-mq --mode <mode> --config <file> --broker http://host:port
```

## Configuration File

```yaml
broker:
  address: "http://localhost:50051"

producer:
  topic: "my-topic"
  partition: 0
  required_acks: 1
  batch_size: 100
  flush_interval_ms: 100

consumer:
  topic: "my-topic"
  partition: 0
  group_id: "my-group"
  offset: -2  # -2=earliest, -1=latest
  auto_commit: true
  poll_interval_ms: 1000
```

## Library API - Producer

```rust
use rust_mq::client::{Producer, ProducerConfig, ProducerMessage};

// Create
let mut producer = Producer::new(broker_addr, config).await?;
producer.start().await?;

// Send
producer.send(ProducerMessage::new(b"data")).await?;

// Sync send
let result = producer.send_sync(msg).await?;

// Shutdown
producer.shutdown().await?;
```

## Library API - Consumer

```rust
use rust_mq::client::{Consumer, ConsumerConfig, MessageHandler};

// Define handler
struct MyHandler;

#[async_trait::async_trait]
impl MessageHandler for MyHandler {
    async fn handle(&mut self, msg: ConsumedMessage) -> Result<()> {
        println!("{:?}", msg.value_as_string()?);
        Ok(())
    }
}

// Create
let mut consumer = Consumer::new(broker_addr, config).await?;

// Start
consumer.start(MyHandler).await?;

// Shutdown
consumer.shutdown().await?;
```

## Configuration Options

### Producer
- `topic`: Target topic
- `partition`: Partition number
- `required_acks`: -1 (all) | 0 (none) | 1 (leader)
- `timeout_ms`: Request timeout
- `batch_size`: Messages per batch
- `flush_interval_ms`: Auto-flush interval

### Consumer
- `topic`: Source topic
- `partition`: Partition number
- `group_id`: Consumer group
- `offset`: -2 (earliest) | -1 (latest) | 0+ (specific)
- `max_bytes`: Max fetch size
- `auto_commit`: Enable auto-commit
- `auto_commit_interval_ms`: Commit frequency
- `poll_interval_ms`: Polling frequency

## File Structure

```
src/
├── main.rs              # CLI entrypoint
├── lib.rs               # Library entrypoint
├── client/
│   ├── config.rs       # Configuration
│   ├── producer.rs     # Producer implementation
│   └── consumer.rs     # Consumer implementation
└── broker/
    ├── core.rs         # Broker logic
    └── storage.rs      # Storage layer

config/                  # Example configs
examples/                # Usage examples
```

## Logging

```bash
# Set level
RUST_LOG=info cargo run -- --mode broker
RUST_LOG=debug cargo run -- --mode consumer

# Filter by module
RUST_LOG=rust_mq::client=debug cargo run
```

## Common Patterns

### Event Publisher
```rust
let mut producer = Producer::new(addr, config).await?;
producer.start().await?;

for event in events {
    producer.send(ProducerMessage::new(serialize(event))).await?;
}

producer.shutdown().await?;
```

### Event Subscriber
```rust
struct EventProcessor;

#[async_trait::async_trait]
impl MessageHandler for EventProcessor {
    async fn handle(&mut self, msg: ConsumedMessage) -> Result<()> {
        let event = deserialize(&msg.value)?;
        process(event).await
    }
}

let mut consumer = Consumer::new(addr, config).await?;
consumer.start(EventProcessor).await?;
```

### Manual Polling
```rust
let mut consumer = Consumer::new(addr, config).await?;

loop {
    let messages = consumer.poll().await?;
    for msg in messages {
        process(msg)?;
    }
    consumer.commit().await?;
}
```

## Troubleshooting

| Problem | Solution |
|---------|----------|
| Can't connect | Check broker is running |
| No messages | Check offset (`-2` for earliest) |
| Not committing | Enable `auto_commit: true` |
| Slow performance | Increase `batch_size` |

## Performance Tuning

```yaml
# High throughput producer
producer:
  batch_size: 1000
  flush_interval_ms: 500
  required_acks: 0

# High throughput consumer
consumer:
  max_bytes: 10485760  # 10MB
  poll_interval_ms: 100
```

## Documentation

- **README.md** - Overview and quick start
- **USAGE_GUIDE.md** - Complete usage guide
- **CLIENT_LIBRARY.md** - Full API reference
- **BROKER_IMPLEMENTATION.md** - Architecture details
- **IMPLEMENTATION_SUMMARY.md** - Implementation details

## Links

- Example: `cargo run --example producer_consumer`
- Tests: `cargo test`
- Build: `cargo build --release`
- Format: `cargo fmt`
- Lint: `cargo clippy`
