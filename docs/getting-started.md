# Getting Started

This guide walks through building Rust-MQ from source and running a basic producer/consumer example.

## Prerequisites

- Rust toolchain (stable) — install via [rustup](https://rustup.rs/)
- `protoc` (Protocol Buffer compiler) — required for gRPC code generation

```bash
# Install protoc on Ubuntu/Debian
sudo apt install -y protobuf-compiler

# Install protoc on macOS
brew install protobuf
```

## Build

```bash
git clone <repository-url>
cd Rust-MQ

cargo build --release
```

The first build compiles protobuf definitions and all dependencies, which may take a few minutes.

## Run a Single-Node Broker

Start a broker with default settings (listens on `localhost:50051`):

```bash
cargo run -- --mode broker
```

Or with a config file:

```bash
cargo run -- --mode broker --config config/broker-1.yaml
```

The broker is ready when you see a log line indicating it is listening.

## Run the Example

The bundled example spins up a broker, sends 20 messages, and consumes them with a custom handler — all in a single process:

```bash
cargo run --example producer_consumer
```

Expected output:

```
[Producer] Sent message #0: "Hello, Rust-MQ! #0"
[Producer] Sent message #1: "Hello, Rust-MQ! #1"
...
[Consumer] Received message #0 at offset 0: "Hello, Rust-MQ! #0"
[Consumer] Received message #1 at offset 1: "Hello, Rust-MQ! #1"
...
[Stats] Sent: 20, Received: 20
```

## Run a Producer from the CLI

With a broker already running, start an interactive producer that reads lines from stdin:

```bash
cargo run -- --mode producer --config config/producer.yaml
```

Type a message and press Enter to send it. Press `Ctrl+C` to exit.

## Run a Consumer from the CLI

In another terminal, start a consumer that prints received messages:

```bash
cargo run -- --mode consumer --config config/consumer.yaml
```

The consumer will print each message it receives, including its topic, partition, and offset.

## Run Benchmarks

```bash
# Quick in-process benchmark
cargo run --release --example benchmark

# Criterion statistical benchmarks
cargo bench
```

See [Benchmarking](./development/benchmarking.md) for how to interpret results.

## Next Steps

- Read [Concepts](./concepts.md) to understand topics, partitions, and offsets.
- See [Configuration](./configuration.md) for all available options.
- See [Cluster Deployment](./deployment/cluster.md) to run a fault-tolerant 3-node cluster.
- See [API Reference](./api/) to integrate Rust-MQ into your application.
