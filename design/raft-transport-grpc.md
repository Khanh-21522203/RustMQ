## Raft Transport gRPC

### Purpose

Transport Raft protocol messages between controller nodes using tonic gRPC.

### Scope

**In scope:**
- Outbound sender `RaftNetworkSender` and `GrpcTransport` in [`src/broker/grpc/transport.rs`](../src/broker/grpc/transport.rs).
- Inbound `RaftGrpcServer` and `RaftServiceImpl` in [`src/broker/grpc/server.rs`](../src/broker/grpc/server.rs).
- Raft proto contract in [`src/api/raft.proto`](../src/api/raft.proto).

**Out of scope:**
- Broker client API gRPC service (`kafka.proto`).
- SBE TCP alternative transport.

### Primary User Flow

1. Controller node has outbound Raft `Message` values from `RawNode`.
2. Transport sender resolves peer RPC addresses and sends `SendRaft` gRPC calls.
3. Receiving peer decodes message bytes and pushes into `step_tx` channel.
4. Local Raft node consumes stepped messages.

### System Flow

1. Outbound:
- `GrpcTransport::send_messages` delegates to `RaftNetworkSender::send_messages`.
- For each message, skip self, resolve peer from `HashMap<u64, PeerInfo>`.
- Encode `eraftpb::Message` bytes with prost (`prost_raft`).
- Send `raft_proto::RaftMessage` through cached `RaftClient`.
2. Inbound:
- `RaftGrpcServer::serve` binds tonic server.
- `send_raft` decodes incoming bytes to `eraftpb::Message` and forwards to `step_tx`.

### Data Model

- `PeerInfo { rpc_addr: String, api_addr: String, sbe_tcp_addr: Option<String> }`.
- `RaftMessage { rpc_type: string, data: bytes }`.
- `RaftReply { data: bytes, error: string }`.

Persistence behavior:
- Transport itself is stateless except in-memory gRPC client connection pool.

### Interfaces and Contracts

- Transport trait contract: `RaftTransport::send_messages(Vec<Message>)`.
- Proto service contract:
- `service Raft { rpc SendRaft(RaftMessage) returns (RaftReply); }`.

### Dependencies

**Internal modules:**
- Shared peer map from controller runtime.
- `raft_transport` trait abstraction.

**External services/libraries:**
- `tonic` gRPC client/server.
- `prost` via `prost_raft` encode/decode.

### Failure Modes and Edge Cases

- Missing peer mapping logs warning and drops message.
- Connect/send failures log warnings and do not panic.
- Decode failure on inbound message returns `Status::invalid_argument`.

### Observability and Debugging

- Server start log: Raft RPC listening address.
- Warnings on no peer, connect failure, send failure, decode failure.
- Debug peer map mismatches by inspecting `PeerInfo.rpc_addr` values.

### Risks and Notes

- `rpc_type` field in `RaftMessage` is currently not validated/used.
- Connection pool has no active health eviction; stale channels depend on tonic reconnect behavior.

Changes:

