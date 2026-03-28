## Cluster Membership and Join

### Purpose

Manage dynamic broker membership through `AddNode`/`RemoveNode` RPCs and startup-time join logic that follows controller redirects.

### Scope

**In scope:**
- `handle_add_node` and `handle_remove_node` in [`src/broker/server/core.rs`](../src/broker/server/core.rs).
- Membership RPC routing in [`src/broker/server/rpc_router.rs`](../src/broker/server/rpc_router.rs) and gRPC method mapping in [`src/broker/server/kafka_server.rs`](../src/broker/server/kafka_server.rs).
- Join retry loop in `run_kraft_cluster` in [`src/main.rs`](../src/main.rs).
- Storage trait defaults/overrides for `add_node`/`remove_node`.

**Out of scope:**
- Controller internals that apply conf changes to Raft state.
- General producer/consumer data path.

### Primary User Flow

1. New broker starts with `join_addr` in config.
2. Node attempts `AddNode` to target API address.
3. If target is not controller, broker receives redirect (`error_code = 6`, `error_message = leader_addr`) and retries against leader.
4. Existing members can remove node with `RemoveNode`.

### System Flow

1. Startup path in `run_kraft_cluster` spawns background join task when `join_addr` is set.
2. Join task tries up to 10 attempts:
- Connect using `KafkaBrokerClient::new(target)`.
- Send `AddNodeRequest { node_id, api_addr, rpc_addr }`.
- On success (`error_code == 0`) stop.
- On redirect (`error_code == 6`) replace target and continue.
3. Server path:
- RPC enters `BrokerRpcRouter::dispatch`.
- `handle_add_node`/`handle_remove_node` call storage `add_node`/`remove_node`.
- `NOT_LEADER` errors are parsed into redirect responses.

### Data Model

- `AddNodeRequest { node_id: u64, api_addr: String, rpc_addr: String }`.
- `AddNodeResponse { error_code: i32, error_message: String }`.
- `RemoveNodeRequest { node_id: u64 }`.
- `RemoveNodeResponse { error_code: i32, error_message: String }`.
- Internal error translation uses `BrokerError::from_message` and `leader_addr()`.

Persistence behavior:
- Membership changes are persisted by controller metadata layer; this module only forwards commands and returns status.

### Interfaces and Contracts

- RPC contracts from [`src/api/kafka.proto`](../src/api/kafka.proto): `AddNode`, `RemoveNode`.
- Redirect contract used by runtime:
- `error_code = 6` means caller should retry against `error_message` address.

### Dependencies

**Internal modules:**
- Storage trait implementor (`KRaftBroker` in cluster mode).
- Client RPC wrapper for join task (`src/client/kafka_broker_client.rs`).

**External services/libraries:**
- `tokio` background task and timers for retry loop.

### Failure Modes and Edge Cases

- Single-node storage default returns `add_node/remove_node is only supported in cluster mode`.
- Non-leader detection partially relies on string matching (`"not leader"`) in addition to parsed `BrokerError`.
- Join task logs failure after max attempts and exits silently from startup perspective (broker still running).

### Observability and Debugging

- Router logs `recv AddNode` and `recv RemoveNode` at info level.
- Join task logs connection failures, RPC failures, redirects, and final success/failure.
- Debug redirects by tracing `error_code` and `error_message` in add/remove responses.

### Risks and Notes

- `error_message` is overloaded for two meanings: redirect target and free-form error text.
- No authorization controls on membership RPCs are visible in current code.

Changes:

- Replace overloaded `error_message` redirect signaling with a dedicated `leader_addr` field in add/remove responses.
- Add authorization checks for `AddNode` and `RemoveNode` operations.
- Add jittered backoff and readiness checks in the startup join loop.
