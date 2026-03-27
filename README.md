# Rust-MQ

A Kafka-like message queue system written in Rust with both CLI and library support.

## Features

- **Multiple Modes**: Run as broker, producer, or consumer
- **Multi-Broker Support**: Distributed cluster with Raft consensus
- **High Availability**: Automatic leader election and failover
- **High Performance**: 800K+ msg/s throughput, <10ms latency
- **Pluggable Raft Transport**: gRPC (default) or SBE+TCP (5.4× faster)
- **Zero-Allocation Codec**: Hand-rolled SBE encoding + lock-free buffer pool on the SBE+TCP path
- **YAML Configuration**: Easy configuration management
- **Reusable Library**: Use as a library in your Rust projects
- **CLI Support**: Run as standalone tools
- **Graceful Shutdown**: Proper SIGINT/SIGTERM handling
- **Batch Processing**: Automatic message batching (2M+ msg/s with large batches)
- **Auto-commit**: Automatic offset management
- **Consumer Groups**: Offset tracking and load balancing
- **SOLID Architecture**: Clean, maintainable codebase

## Architecture

Rust-MQ supports two deployment modes:

- **Single Broker**: Simple in-memory storage for development and testing
- **Multi-Broker Cluster**: Distributed system with Raft consensus for production

See [docs/architecture.md](docs1/architecture.md) for detailed architecture documentation with diagrams.  

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
        partitioning: "fixed".to_string(),
        num_partitions: 1,
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
        partitions: vec![0],
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

### Structured Payload Encoding/Decoding

Rust-MQ stores message values as raw bytes. You can use the built-in codec
module to wrap your payload with a simple versioned envelope:

```rust
use serde::{Deserialize, Serialize};
use rust_mq::codec::{MessageEnvelope, encode, decode};
use rust_mq::client::ProducerMessage;

#[derive(Debug, Serialize, Deserialize)]
struct OrderCreated {
    order_id: String,
    amount_cents: u64,
}

// Encode before produce
let envelope = MessageEnvelope::new(
    "order.created",
    1,
    OrderCreated { order_id: "ORD-1001".into(), amount_cents: 1250 },
);
let bytes = encode(&envelope)?; // compact binary (bincode)
producer.send(ProducerMessage::new(bytes)).await?;

// Decode after consume
let decoded: MessageEnvelope<OrderCreated> = decode(&consumed_message.value)?;
println!("event={} order={}", decoded.event_type, decoded.payload.order_id);
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
  partitioning: "fixed"
  num_partitions: 1
  required_acks: 1
  timeout_ms: 5000
  batch_size: 100
  flush_interval_ms: 100

consumer:
  topic: "events"
  partitions: [0]
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
│   ├── main.rs                     # CLI entry point
│   ├── lib.rs                      # Library entry point
│   ├── api/                        # Protobuf definitions
│   ├── broker/                     # Broker implementation
│   │   ├── core.rs                # Broker logic
│   │   ├── storage.rs             # Storage layer
│   │   ├── raft_transport.rs      # RaftTransport trait
│   │   └── sbe_tcp/               # SBE + TCP transport
│   │       ├── codec.rs           # Hand-rolled SBE encoder/decoder
│   │       ├── pool.rs            # Lock-free buffer pool
│   │       ├── connection.rs      # Persistent TCP connection manager
│   │       ├── server.rs          # TCP inbound server
│   │       └── transport.rs       # RaftTransport impl
│   └── client/                     # Client library
│       ├── config.rs              # Configuration
│       ├── producer.rs            # Producer implementation
│       ├── consumer.rs            # Consumer implementation
│       └── kafka_broker_client.rs # gRPC client
├── config/
│   ├── docker/                     # gRPC cluster configs
│   └── docker/sbe-tcp/             # SBE+TCP cluster configs
├── docker-compose.yml              # gRPC cluster
├── docker-compose-sbe-tcp.yml      # SBE+TCP cluster
├── examples/                       # Usage examples
└── docs/                           # Documentation
```

### Raft Transport Configuration

Add `transport` to your broker YAML to switch inter-broker communication:

```yaml
# gRPC (default — works out of the box, no extra config needed)
transport: "grpc"

# SBE + TCP (recommended for production — 5.4× faster than gRPC)
transport: "sbe_tcp"
```
## Testing

```bash
# Run all tests
cargo test

# Run with logging
RUST_LOG=debug cargo test

# Run specific example
cargo run --example producer_consumer

# Run performance benchmarks
cargo run --release --example benchmark

# Run Criterion benchmarks (detailed statistical analysis)
cargo bench
```

## Environment Variables

```bash
# Set log level
RUST_LOG=info cargo run -- --mode broker
RUST_LOG=debug cargo run -- --mode consumer

# Available levels: error, warn, info, debug, trace
```

## Performance Benchmarks

### Test Environment
- **Mode**: Single broker (in-memory storage)
- **Hardware**: Local development machine
- **Build**: Release mode (`--release`)

### Results Summary

#### Producer Throughput
| Message Size | Throughput | Data Rate |
|--------------|------------|-----------|
| 100B | 805,137 msg/s | 76.78 MB/s |
| 1KB | 368,357 msg/s | 351.29 MB/s |
| 10KB | 70,707 msg/s | 674.31 MB/s |

#### Batch Size Impact (100B messages)
| Batch Size | Throughput |
|------------|------------|
| 10 | 105,743 msg/s |
| 100 | 724,819 msg/s |
| 1000 | 2,012,116 msg/s |

#### End-to-End Latency
- **Average**: 9.93ms
- **Min**: 2.52ms
- **Max**: 12.04ms
- **p50**: 9.93ms
- **p95**: 11.91ms
- **p99**: 12.04ms

#### Consumer Throughput
- **Rate**: 100 msg/s (10,000 messages test)

### Running Benchmarks

```bash
# Run the benchmark suite
cargo run --release --example benchmark

# Run Criterion benchmarks (detailed analysis)
cargo bench
```

### Docker Compose Benchmark (3-node Raft cluster)

Three transport backends are supported. Use the matching compose file for each:

```bash
BENCH='N=50000; START=$(date +%s%3N); seq 1 "$N" | sed "s/^/bench-msg-/" \
  | rust-mq --mode producer --config /app/config/producer.yaml --broker http://broker-1:9092 \
    >/tmp/bench.stdout 2>/tmp/bench.stderr; RC=$?; END=$(date +%s%3N); DUR_MS=$((END-START)); \
  THR=$(awk -v n="$N" -v d="$DUR_MS" "BEGIN{printf \"%.2f\",(n*1000.0)/d}"); \
  echo "RC=$RC N=$N DUR_MS=$DUR_MS THROUGHPUT_MSG_PER_SEC=$THR"'

# gRPC (default)
docker compose up -d --build
docker compose exec -T broker-1 sh -lc "$BENCH"
docker compose down -v

# SBE + TCP
docker compose -f docker-compose-sbe-tcp.yml up -d --build
docker compose -f docker-compose-sbe-tcp.yml exec -T broker-1 sh -lc "$BENCH"
docker compose -f docker-compose-sbe-tcp.yml down -v
```

#### Results (March 26, 2026 — Docker bridge network, 50 000 messages, batch_size=100)

| Transport | Duration | Throughput | vs gRPC | Errors |
|-----------|----------|------------|---------|--------|
| gRPC (default) | 20 017 ms | **2 497 msg/s** | 1× | 0 |
| SBE + TCP | 3 719 ms | **13 444 msg/s** | **5.4×** | 0 |

**SBE + TCP** eliminates HTTP/2 framing and Protobuf encoding from the Raft inter-broker path, replacing them with a hand-rolled SBE codec and a lock-free buffer pool — **5.4× throughput improvement** with zero message loss.

## Acknowledgments

- Inspired by Apache Kafka
- Built with Rust, Tokio, and Tonic (gRPC)
