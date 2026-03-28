## Consumer Group Coordination

### Purpose

Implement broker-side group membership operations (`FindCoordinator`, `JoinGroup`, `SyncGroup`, `Heartbeat`, `LeaveGroup`) and partition assignment/rebalance behavior.

### Scope

**In scope:**
- Group RPC handlers in [`src/broker/server/core.rs`](../src/broker/server/core.rs).
- Group state logic in [`src/broker/server/consumer_group.rs`](../src/broker/server/consumer_group.rs).
- In-memory storage-backed group implementation in [`src/broker/storage/traits.rs`](../src/broker/storage/traits.rs).

**Out of scope:**
- Client-side consumer polling/handler runtime.
- Offset commit/fetch data structures.

### Primary User Flow

1. Consumer sends `FindCoordinator` for its `group_id`.
2. Consumer sends `JoinGroup` with protocol metadata (topic in current implementation).
3. Consumer sends `SyncGroup` to receive assignment bytes.
4. Consumer periodically sends `Heartbeat`.
5. Consumer sends `LeaveGroup` on shutdown.

### System Flow

1. `handle_find_coordinator` returns local coordinator host/port from storage.
2. `handle_join_group` forwards first `group_protocols` entry metadata and timeout to storage/coordinator.
3. Coordinator state machine:
- Track members and heartbeat timestamps.
- Trigger rebalance when membership changes.
- Finalize rebalance when all members rejoin or timeout elapses.
- Compute round-robin assignments from topic partition counts.
4. `handle_sync_group` returns bincode-serialized partition list (`Vec<i32>`) for member.
5. Background task every 5s expires timed-out members and finalizes stalled rebalances.

```text
JoinGroup
  -> group state update
     -> first member -> Stable + assignment
     -> membership change -> PreparingRebalance
        -> Sync/Heartbeat may return rebalance-in-progress
```

### Data Model

- Internal group state (`consumer_group.rs`):
- `GroupState { generation_id, leader_id, members, subscriptions, assignments, status, rejoined, session_timeout_ms, rebalance_started_ms }`.
- `GroupMemberState { member_id, metadata, last_heartbeat_ms }`.
- Status enum: `Empty`, `Stable`, `PreparingRebalance`.
- API structures:
- `JoinGroupRequest/Response`, `SyncGroupRequest/Response`, `HeartbeatRequest/Response`, `LeaveGroupRequest/Response`.

Persistence behavior:
- Group state is in-memory only in current implementations.

### Interfaces and Contracts

- RPC contracts in [`src/api/kafka.proto`](../src/api/kafka.proto): `FindCoordinator`, `JoinGroup`, `SyncGroup`, `Heartbeat`, `LeaveGroup`.
- Current assignment payload contract: `SyncGroupResponse.member_assignment` contains bincode-encoded `Vec<i32>` partition ids.

### Dependencies

**Internal modules:**
- Topic partition counts from storage/coordinator topic map (`update_topic`).
- Shared `BrokerError` values (`RebalanceInProgress`, `NotFound`).

**External services/libraries:**
- `bincode` for assignment serialization.

### Failure Modes and Edge Cases

- Unknown group/member produces `BrokerError::NotFound` -> core maps to generic error code `1`.
- Rebalance in progress maps to error code `27` in core for sync/heartbeat.
- `generation_id`, `protocol_type`, and `group_assignment` are minimally used in current implementation.
- Membership expiry is timer-driven; abrupt disconnects remain until timeout/leave.

### Observability and Debugging

- Router logs join/leave at info, sync/heartbeat at debug.
- Core logs join/sync/heartbeat/leave errors.
- Group algorithm debug starts in [`src/broker/server/consumer_group.rs`](../src/broker/server/consumer_group.rs).

### Risks and Notes

- Group coordination logic exists in both `consumer_group.rs` and `storage/traits.rs`; duplication can diverge.
- Rebalance error code in core (`27`) does not match `REBALANCE_IN_PROGRESS = 14` in `kafka.proto` enum.

Changes:

- Align rebalance error mapping with `kafka.proto` (`REBALANCE_IN_PROGRESS = 14`) across broker and client paths.
- Consolidate duplicated group-state logic into a single coordinator implementation to avoid divergence.
- Enforce `generation_id` and protocol validation for `JoinGroup`, `SyncGroup`, and `Heartbeat`.
