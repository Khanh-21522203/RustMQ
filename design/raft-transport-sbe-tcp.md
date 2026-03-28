## Raft Transport SBE TCP

### Purpose

Transport Raft messages over raw TCP using a custom SBE framing/encoding path.

### Scope

**In scope:**
- Outbound `SbeTcpTransport` and `ConnectionManager` in [`src/broker/sbe_tcp/transport.rs`](../src/broker/sbe_tcp/transport.rs) and [`src/broker/sbe_tcp/connection.rs`](../src/broker/sbe_tcp/connection.rs).
- Inbound `SbeTcpServer` in [`src/broker/sbe_tcp/server.rs`](../src/broker/sbe_tcp/server.rs).
- Wire codec in [`src/broker/sbe_tcp/codec.rs`](../src/broker/sbe_tcp/codec.rs).

**Out of scope:**
- gRPC transport path.
- Broker client-facing API protocols.

### Primary User Flow

1. Controller emits outbound Raft messages.
2. Transport encodes each message into SBE payload, prefixes frame length, and queues it per peer connection.
3. Writer task sends frames on persistent TCP connection (reconnect on failure).
4. Receiver accepts frames, decodes SBE payload to Raft message, and forwards to Raft step channel.

### System Flow

1. Outbound:
- `SbeTcpTransport::send_messages` -> `ConnectionManager::send_messages`.
- Resolve peer address from shared `PeerInfo` map (`sbe_tcp_addr` first, fallback to stripped `rpc_addr`).
- Build frame: 4-byte LE payload length + SBE payload.
- Send via bounded per-peer channel to `writer_task`.
2. `writer_task`:
- Connect/reconnect loop per peer.
- Write frames to TCP stream; on error, reconnect.
3. Inbound server:
- `SbeTcpServer::serve` accepts peer TCP sockets.
- `handle_peer` reads length, rejects frames above max size, then reads payload, decodes via `codec::decode`, sends to `step_tx`.

### Data Model

- Frame format:
- `u32_le payload_len`
- `payload_len` bytes of SBE-encoded `eraftpb::Message`.
- SBE fixed block fields include `msg_type`, `to`, `from`, `term`, `log_term`, `index`, `commit`, `commit_term`, `request_snapshot`, `reject`, `reject_hint`, `priority`, entry and payload lengths.

Persistence behavior:
- Connection cache and send queues are in-memory; no transport persistence.

### Interfaces and Contracts

- Transport trait contract: `RaftTransport::send_messages(Vec<Message>)`.
- Codec contract:
- `encode(msg, &mut BytesMut)` writes SBE payload only.
- `decode(&[u8]) -> Message` decodes one payload.

### Dependencies

**Internal modules:**
- Buffer pooling in [`src/broker/sbe_tcp/pool.rs`](../src/broker/sbe_tcp/pool.rs).
- Shared peer map from controller startup path.

**External services/libraries:**
- `tokio` TCP and async channels.
- `bytes` for buffer building.
- `prost_raft` for snapshot prost bytes in codec.

### Failure Modes and Edge Cases

- Unknown peer id logs warning and drops message.
- Send channel full or closed removes cached peer connection.
- Decode errors on inbound frames are logged and connection stays open for subsequent frames.
- Inbound frame length above max-size guard is rejected before allocation/decode.

### Observability and Debugging

- `[sbe-tcp]` log prefix across connect/reconnect/read/write/decode paths.
- Debug wire issues by checking length-prefix framing and codec test coverage in `codec.rs` tests.

### Risks and Notes

- `PeerInfo.sbe_tcp_addr` is now used as the primary outbound SBE address.
- No TLS/authentication at transport layer in current implementation.

Changes:
