## Broker RPC Ingress and Dispatch

### Purpose

Expose Kafka-like broker operations over gRPC and route each RPC to broker core handlers through a channel-based dispatch layer.

### Scope

**In scope:**
- `broker.Broker` gRPC surface from [`src/api/kafka.proto`](../src/api/kafka.proto).
- `KafkaBrokerServer` in [`src/broker/server/kafka_server.rs`](../src/broker/server/kafka_server.rs).
- Request/response enums in [`src/api/requests.rs`](../src/api/requests.rs) and [`src/api/responses.rs`](../src/api/responses.rs).
- `BrokerCore::run` and `BrokerRpcRouter::dispatch` in [`src/broker/server/core.rs`](../src/broker/server/core.rs) and [`src/broker/server/rpc_router.rs`](../src/broker/server/rpc_router.rs).

**Out of scope:**
- Storage-specific logic inside individual handler methods.
- Raft inter-node transport APIs (`src/api/raft.proto`).

### Primary User Flow

1. Client calls a broker gRPC method (for example `Produce`).
2. gRPC server packs it into `BrokerGrpcRequest::<Variant>` and sends over `mpsc`.
3. Broker core receives the request, routes by variant, and builds a typed response.
4. Response is sent back through `oneshot` to the gRPC layer and returned to caller.

### System Flow

1. Entry point: `KafkaBrokerServer::serve` binds tonic server.
2. Each RPC method uses `dispatch!` macro:
- Send `(BrokerGrpcRequest, oneshot::Sender<BrokerGrpcResponse>)` into `rpc_send_channel`.
- Await `oneshot` reply.
3. `BrokerCore::run` loops on `rpc_receive_channel.recv()`.
4. `BrokerCore::handle_rpc` creates `BrokerRpcRouter` and calls `dispatch`.
5. Router matches variant and calls matching `BrokerCore::handle_*` method.

```text
Client RPC
  -> KafkaBrokerServer (tonic)
     -> mpsc send BrokerGrpcRequest
        -> BrokerCore::run
           -> BrokerRpcRouter::dispatch
              -> BrokerCore::handle_*
        -> oneshot BrokerGrpcResponse
```

### Data Model

- `BrokerGrpcRequest` enum variants:
- `GetTopicMetadata`, `CreateTopic`, `Produce`, `Fetch`, `ListOffsets`.
- `FindCoordinator`, `JoinGroup`, `SyncGroup`, `Heartbeat`, `LeaveGroup`.
- `CommitOffset`, `FetchOffset`, `AddNode`, `RemoveNode`.
- `BrokerGrpcResponse` mirrors these variants.
- Channel contract:
- Ingress queue: `mpsc::Sender<(BrokerGrpcRequest, oneshot::Sender<BrokerGrpcResponse>)>`.

Persistence behavior:
- This layer is stateless transport/dispatch glue.

### Interfaces and Contracts

- Public RPC interface: `service Broker` in [`src/api/kafka.proto`](../src/api/kafka.proto).
- gRPC status behavior in dispatch macro:
- Channel send failure -> `Status::internal("broker unavailable: ...")`.
- Reply channel canceled -> `Status::internal("broker did not respond")`.
- Variant mismatch -> `Status::internal("unexpected response variant")`.

### Dependencies

**Internal modules:**
- Broker core and router modules.
- API request/response enum modules.

**External services/libraries:**
- `tonic` for gRPC server.
- `tokio::sync::{mpsc, oneshot}` for async dispatch plumbing.

### Failure Modes and Edge Cases

- If broker core task exits, RPC layer returns internal errors due to closed channel.
- If a handler panics and drops reply path, caller sees `broker did not respond`.
- Manual enum mapping means new RPCs require synchronized changes in proto, enums, server trait impl, and router.

### Observability and Debugging

- `KafkaBrokerServer::serve` logs listen address.
- Router logs request summaries (`recv Produce`, `recv Fetch`, group/member IDs for group ops).
- Debugging mismatch bugs starts at `dispatch!` macro and `BrokerRpcRouter::dispatch`.

### Risks and Notes

- No request correlation IDs or structured tracing spans; only method-level logs.
- Backpressure behavior depends on channel capacity configured by caller (`mpsc::channel(1000)` in startup paths).

Changes:
