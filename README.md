# Rust-MQ

A Kafka-like message queue system written in Rust with both CLI and library support.

## Features

✅ **Multiple Modes**: Run as broker, producer, or consumer  
✅ **Multi-Broker Support**: Distributed cluster with Raft consensus (NEW!)  
✅ **High Availability**: Automatic leader election and failover  
✅ **YAML Configuration**: Easy configuration management  
✅ **Reusable Library**: Use as a library in your Rust projects  
✅ **CLI Support**: Run as standalone tools  
✅ **Graceful Shutdown**: Proper SIGINT/SIGTERM handling  
✅ **Batch Processing**: Automatic message batching  
✅ **Auto-commit**: Automatic offset management  
✅ **Consumer Groups**: Offset tracking and load balancing  
✅ **SOLID Architecture**: Clean, maintainable codebase

## Architecture

Rust-MQ supports two deployment modes:

- **Single Broker**: Simple in-memory storage for development and testing
- **Multi-Broker Cluster**: Distributed system with Raft consensus for production

See [ARCHITECTURE.md](ARCHITECTURE.md) for detailed architecture documentation with diagrams.  

## Quick Start

### 1. Build the Project

```bash
cargo build --release
```

### 2. Run Broker

**Single Broker Mode:**
```bash
# Terminal 1
cargo run -- --mode broker
```

**Multi-Broker Cluster (3 nodes):**
```bash
# Terminal 1 - Broker 1 (Bootstrap)
./target/release/Rust-MQ --mode broker --config config/broker-1.yaml

# Terminal 2 - Broker 2
./target/release/Rust-MQ --mode broker --config config/broker-2.yaml

# Terminal 3 - Broker 3
./target/release/Rust-MQ --mode broker --config config/broker-3.yaml
```

Or use the convenience script:
```bash
./scripts/start-cluster.sh
```

### 3. Run Consumer

```bash
# Terminal 2
cargo run -- --mode consumer --config config/consumer.yaml
```

### 4. Send Messages

```bash
# Terminal 3
cargo run -- --mode producer --config config/producer.yaml
# Type messages line by line
```

## CLI Usage

### Command-Line Interface

```bash
rust-mq --mode <MODE> [--config <CONFIG>] [--broker <BROKER>]

Options:
  -m, --mode <MODE>        broker, producer, or consumer
  -c, --config <CONFIG>    Path to YAML configuration file
      --broker <BROKER>    Override broker address
  -h, --help              Print help
```

### Examples

```bash
# Start broker
cargo run -- --mode broker

# Producer with config
cargo run -- --mode producer --config config/producer.yaml

# Consumer with config
cargo run -- --mode consumer --config config/consumer.yaml

# Override broker address
cargo run -- --mode consumer --config config/consumer.yaml --broker http://192.168.1.100:50051
```

## Library Usage

### Producer Example

```rust
use rust_mq::client::{Producer, ProducerConfig, ProducerMessage};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = ProducerConfig {
        topic: "my-topic".to_string(),
        partition: 0,
        required_acks: 1,
        timeout_ms: 5000,
        batch_size: 100,
        flush_interval_ms: 100,
    };
    
    let mut producer = Producer::new("http://localhost:50051", config).await?;
    producer.start().await?;
    
    // Send messages
    producer.send(ProducerMessage::new(b"Hello, Kafka!")).await?;
    
    producer.shutdown().await?;
    Ok(())
}
```

### Consumer Example

```rust
use rust_mq::client::{Consumer, ConsumerConfig, ConsumedMessage, MessageHandler};

struct MyHandler;

#[async_trait::async_trait]
impl MessageHandler for MyHandler {
    async fn handle(&mut self, message: ConsumedMessage) -> anyhow::Result<()> {
        println!("Received: {:?}", message.value_as_string()?);
        Ok(())
    }
}

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
        auto_commit: true,
        auto_commit_interval_ms: 5000,
        poll_interval_ms: 1000,
    };
    
    let mut consumer = Consumer::new("http://localhost:50051", config).await?;
    consumer.start(MyHandler).await?;
    
    tokio::signal::ctrl_c().await?;
    consumer.shutdown().await?;
    Ok(())
}
```

## Configuration

Example configuration files are provided in the `config/` directory:

- `config/producer.yaml` - Producer configuration
- `config/consumer.yaml` - Consumer configuration
- `config/example-full.yaml` - Complete configuration example

### Configuration Structure

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

## Examples

### Run the Complete Example

```bash
# Demonstrates producer-consumer communication
cargo run --example producer_consumer
```

Output:
```
=== Rust-MQ Producer-Consumer Example ===

Step 1: Starting broker...
✓ Broker started at 0.0.0.0:50051

Step 2: Creating producer...
✓ Producer started

Step 3: Creating consumer...
✓ Consumer started

Step 4: Sending messages...
[Producer] Sent: Message number 1
[Consumer] Message #1: [demo-topic:0:0] Message number 1
...
```

## Architecture

```
┌─────────────────────────────────────────┐
│           CLI Layer (main.rs)           │
│    Mode selection, argument parsing     │
└──────────────────┬──────────────────────┘
                   │
┌──────────────────▼──────────────────────┐
│    Configuration Layer (config.rs)      │
│      YAML parsing, validation           │
└──────────────────┬──────────────────────┘
                   │
┌──────────────────▼──────────────────────┐
│        Client Layer (producer.rs        │
│             consumer.rs)                │
│   Business logic, batching, polling     │
└──────────────────┬──────────────────────┘
                   │
┌──────────────────▼──────────────────────┐
│          gRPC Client Layer              │
│    (kafka_broker_client.rs)             │
└──────────────────┬──────────────────────┘
                   │
┌──────────────────▼──────────────────────┐
│        Broker (Server Side)             │
│  gRPC Server → Broker Core → Storage    │
└─────────────────────────────────────────┘
```

## Key Design Principles

1. **Separation of Concerns**: CLI, configuration, and business logic are separate
2. **Dependency Injection**: Components accept configuration, not hardcoded values
3. **Interface Segregation**: MessageHandler trait for custom processing
4. **Open/Closed Principle**: Easy to extend with new handlers
5. **Graceful Shutdown**: Proper cleanup and resource management
6. **Type Safety**: Leverages Rust's type system for correctness

## Project Structure

```
Rust-MQ/
├── src/
│   ├── main.rs                 # CLI entry point
│   ├── lib.rs                  # Library entry point
│   ├── api/                    # Protobuf definitions
│   ├── broker/                 # Broker implementation
│   │   ├── core.rs            # Broker logic
│   │   ├── storage.rs         # Storage layer
│   │   └── kafka_broker_server.rs  # gRPC server
│   └── client/                 # Client library
│       ├── config.rs          # Configuration
│       ├── producer.rs        # Producer implementation
│       ├── consumer.rs        # Consumer implementation
│       └── kafka_broker_client.rs  # gRPC client
├── config/                     # Example configurations
├── examples/                   # Usage examples
└── docs/                       # Documentation
```

## Documentation

- **[USAGE_GUIDE.md](USAGE_GUIDE.md)** - Complete usage guide with examples
- **[CLIENT_LIBRARY.md](CLIENT_LIBRARY.md)** - Library API reference
- **[BROKER_IMPLEMENTATION.md](BROKER_IMPLEMENTATION.md)** - Broker architecture

## Testing

```bash
# Run all tests
cargo test

# Run with logging
RUST_LOG=debug cargo test

# Run specific example
cargo run --example producer_consumer
```

## Environment Variables

```bash
# Set log level
RUST_LOG=info cargo run -- --mode broker
RUST_LOG=debug cargo run -- --mode consumer

# Available levels: error, warn, info, debug, trace
```

## Common Use Cases

### 1. Event-Driven Microservices

Use producer to publish events and consumer to process them in separate services.

### 2. Log Aggregation

Stream logs from multiple sources to a central broker for processing.

### 3. Data Pipelines

Build ETL pipelines with producer ingesting data and consumer transforming/loading it.

### 4. Message Queue

Use as a traditional message queue for async job processing.

## Performance Tips

1. **Increase batch size** for higher throughput
2. **Use fire-and-forget** (`required_acks: 0`) for non-critical messages
3. **Tune consumer fetch size** with `max_bytes`
4. **Adjust poll intervals** based on your use case

## Troubleshooting

### Connection Issues

```bash
# Ensure broker is running
cargo run -- --mode broker

# Check broker address in config
broker:
  address: "http://localhost:50051"
```

### Messages Not Appearing

```rust
// Ensure producer flushes
producer.flush().await?;

// Start consumer from beginning
consumer_config.offset = -2;
```

### Offset Not Committing

```rust
// Enable auto-commit
consumer_config.auto_commit = true;
```

## Contributing

Contributions are welcome! Please ensure:
- Code follows Rust idioms
- Tests pass: `cargo test`
- Code is formatted: `cargo fmt`
- No clippy warnings: `cargo clippy`

## License

MIT License - See LICENSE file for details

## Author

Khanh Dao

## Acknowledgments

- Inspired by Apache Kafka
- Built with Rust, Tokio, and Tonic (gRPC)
