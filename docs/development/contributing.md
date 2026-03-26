# Contributing

## Development Setup

```bash
git clone <repository-url>
cd Rust-MQ

# Install Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install protoc (required for gRPC code generation)
# Ubuntu/Debian:
sudo apt install -y protobuf-compiler
# macOS:
brew install protobuf

# Build
cargo build
```

## Common Commands

```bash
# Build (debug)
cargo build

# Build (release — much faster at runtime)
cargo build --release

# Run all tests
cargo test

# Run a single test by name
cargo test test_name

# Run tests with debug logging
RUST_LOG=debug cargo test

# Lint
cargo clippy

# Format
cargo fmt

# Check formatting without modifying files
cargo fmt --check
```

## Project Structure

```
src/
├── main.rs                   # CLI entry point
├── lib.rs                    # Library exports
├── utils.rs                  # Shared utilities
├── api/                      # gRPC protocol layer
│   ├── kafka.proto           # Service and message definitions (edit this)
│   ├── broker.rs             # Auto-generated (do not edit directly)
│   ├── requests.rs           # Request enum wrapping all gRPC request types
│   ├── responses.rs          # Response enum wrapping all gRPC response types
│   └── mod.rs
├── broker/                   # Broker implementation
│   ├── storage.rs            # BrokerStorage trait + InMemoryStorage
│   ├── core.rs               # BrokerCore<S> request dispatcher
│   ├── kafka_broker_server.rs # Tonic gRPC server
│   ├── config.rs             # BrokerConfig deserialization
│   ├── multi_broker.rs       # MultiBroker (Raft-backed BrokerStorage)
│   ├── raft.rs               # RaftNode + replicated state machine + RaftStorage
│   ├── raft_network.rs       # Inter-node gRPC communication
│   └── mod.rs
└── client/                   # Client library
    ├── config.rs             # AppConfig, ProducerConfig, ConsumerConfig
    ├── producer.rs           # Producer with batching
    ├── consumer.rs           # Consumer with offset tracking
    ├── kafka_broker_client.rs # gRPC client wrapper
    └── mod.rs
```

## Adding a New gRPC Method

1. Add the `rpc` definition and message types to `src/api/kafka.proto`.
2. Run `cargo build` — `build.rs` regenerates `src/api/broker.rs`.
3. Add variants to `BrokerGrpcRequest` and `BrokerGrpcResponse` in `requests.rs` / `responses.rs`.
4. Implement the handler in `KafkaBrokerServer` (`kafka_broker_server.rs`).
5. Add the dispatch arm in `BrokerCore` (`core.rs`).
6. Add the method to the `BrokerStorage` trait and implement it in both `InMemoryStorage` and `MultiBroker`.

## Adding a New Storage Backend

Implement the `BrokerStorage` trait from `src/broker/storage.rs`:

```rust
#[async_trait]
impl BrokerStorage for MyStorage {
    async fn produce_message(&self, ...) -> Result<i64, String> { ... }
    async fn fetch_messages(&self, ...) -> Result<Vec<StoredMessage>, String> { ... }
    // ... all other required methods
}
```

Then instantiate `BrokerCore<MyStorage>` with your backend.

## Running Examples

```bash
# In-process producer/consumer demo
cargo run --example producer_consumer

# Throughput and latency benchmarks
cargo run --release --example benchmark
```

## Criterion Benchmarks

```bash
# Run all benchmarks and generate an HTML report
cargo bench

# Run a specific benchmark group
cargo bench -- produce_throughput
```

Results are written to `target/criterion/`. Open `target/criterion/report/index.html` in a browser to view interactive charts.

## Testing Guidelines

- Unit tests live in the same file as the code they test (`#[cfg(test)]` modules).
- Integration tests that require a running broker should start one in the test setup.
- Use `RUST_LOG=debug cargo test -- --nocapture` to see log output while debugging.

## Code Style

- Run `cargo fmt` before committing.
- Run `cargo clippy` and address all warnings.
- Prefer explicit error types over `.unwrap()` in library code.
- Keep `BrokerCore` free of transport concerns; keep `KafkaBrokerServer` free of business logic.
