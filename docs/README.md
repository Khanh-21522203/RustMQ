# Rust-MQ Documentation

Welcome to the Rust-MQ documentation. Rust-MQ is a Kafka-inspired message queue system written in Rust, designed for high-throughput, low-latency messaging with optional high-availability through Raft consensus.

## Documentation Index

| Document | Description |
|---|---|
| [Concepts](./concepts.md) | Core messaging concepts: topics, partitions, offsets, consumer groups |
| [Getting Started](./getting-started.md) | Quick start guide — run your first producer and consumer |
| [Architecture](./architecture.md) | System design, components, and request flow |
| [Configuration](./configuration.md) | Full configuration reference for brokers, producers, and consumers |
| [API Reference](./api/) | gRPC API and client library reference |
| [Deployment](./deployment/) | Single-node and multi-broker cluster deployment guides |
| [Development](./development/) | Contributing, benchmarking, and extending the system |

## Quick Navigation

- **New to Rust-MQ?** Start with [Concepts](./concepts.md) then [Getting Started](./getting-started.md).
- **Deploying to production?** See the [Cluster Deployment](./deployment/cluster.md) guide.
- **Building a client?** See the [API Reference](./api/).
- **Contributing?** See the [Development Guide](./development/contributing.md).
