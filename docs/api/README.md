# API Reference

Rust-MQ exposes its functionality through two layers:

| Layer | Description |
|---|---|
| [Rust Client Library](./client-library.md) | `Producer` and `Consumer` types for Rust applications |
| [gRPC API](./grpc.md) | Raw gRPC/protobuf interface for non-Rust clients |

## Which Should I Use?

- **Building a Rust application?** Use the [Client Library](./client-library.md). It handles batching, offset tracking, consumer groups, and reconnection for you.
- **Building a client in another language?** Use the [gRPC API](./grpc.md) directly. The `.proto` definitions are in `src/api/kafka.proto`.
- **Integrating with existing Kafka tooling?** The API is modeled after the Kafka protocol; many concepts map directly, though wire compatibility is not guaranteed.
