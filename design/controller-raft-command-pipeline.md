## Controller Raft Command Pipeline

### Purpose

Replicate cluster metadata commands through Raft, apply them to `ControllerMetadata`, and expose a proposal/read handle (`ControllerStorage`) to broker runtime code.

### Scope

**In scope:**
- `ControllerRaftNode` and `ControllerStorage` in [`src/broker/controller/raft_node.rs`](../src/broker/controller/raft_node.rs).
- State machine command application in [`src/broker/controller/state_machine.rs`](../src/broker/controller/state_machine.rs).
- Metadata types and command enums in [`src/broker/controller/types.rs`](../src/broker/controller/types.rs).

**Out of scope:**
- Broker data log replication (`PartitionLog`, fetch loop).
- Client-facing broker RPC server.

### Primary User Flow

1. Broker startup creates `ControllerRaftNode` and `ControllerStorage`.
2. Leader node receives metadata proposals (`CreateTopic`, `PartitionChange`, broker membership changes, heartbeats).
3. Proposals are committed via Raft and applied to metadata.
4. All nodes read consistent metadata snapshots through `ControllerStorage::metadata()`.

### System Flow

1. `ControllerRaftNode::new_with_transport` configures Raft node, channels, peer map, and optional sled metadata recovery.
2. `ControllerStorage::propose*` methods send `ControllerCommand` via `propose_tx` or `conf_change_tx`.
3. `ControllerRaftNode::run` event loop:
- ticks Raft,
- receives proposals/steps,
- processes `Ready` state,
- sends outbound Raft messages through `RaftTransport`.
4. Committed entries:
- normal entries -> `apply_entry_data` -> `apply_controller_command`.
- conf changes -> `apply_conf_change_entry` -> adjust conf state, peers, and metadata.
5. Applied metadata is persisted to sled key `controller_meta` when DB configured.

```text
ControllerStorage::propose*
  -> ControllerRaftNode::run
     -> RawNode::propose / propose_conf_change
     -> transport.send_messages(...)
     -> committed entry apply
        -> apply_controller_command(meta, cmd)
```

### Data Model

- `ControllerMetadata`:
- `topics: HashMap<String, TopicRecord>`
- `partitions: HashMap<(String, i32), PartitionRecord>`
- `brokers: HashMap<u64, BrokerRegistration>`
- `controller_epoch: u64`
- `ControllerCommand` variants:
- `CreateTopic`, `DeleteTopic`, `PartitionChange`, `RegisterBroker`, `UnregisterBroker`, `BumpControllerEpoch`, `BrokerHeartbeat`, `MarkBrokerDead`.
- Proposal wrapper: `ControllerProposal { proposal_id, command }`.

Persistence behavior:
- Sled keys:
- `controller_meta` for serialized metadata.
- `peers` for serialized peer map.
- Raft storage itself is `MemStorage` (in-memory log/hardstate).

### Interfaces and Contracts

- `ControllerHandle` trait in [`src/broker/kraft/broker.rs`](../src/broker/kraft/broker.rs) implemented by `ControllerStorage`.
- Leader gating contract:
- Non-leader proposals fail with `NOT_LEADER:<leader_api_addr>`.
- One conf change at a time (`pending_cc_reply`).

### Dependencies

**Internal modules:**
- `state_machine` for deterministic metadata transitions.
- `raft_transport` abstraction and concrete transport modules.
- `main.rs` startup wiring.

**External services/libraries:**
- `raft` crate (`RawNode`, `MemStorage`, `ConfChange`).
- `sled` for metadata persistence.
- `bincode` for command/metadata serialization.

### Failure Modes and Edge Cases

- Proposal from follower returns `NOT_LEADER` error.
- Leadership change before commit fails pending proposal replies.
- Conf change decode/apply failures are logged and returned through pending reply channel.
- If sled open fails, runtime continues without persisted metadata.

### Observability and Debugging

- Logs include restored peers/metadata, conf change add/remove events, and apply failures.
- Proposal queue/apply latency is logged in `ControllerRaftNode`:
- warn-level when queue or apply latency exceeds thresholds,
- debug-level latency traces otherwise.
- Debug proposal stalls by checking `pending_replies` clearing on leadership transitions.
- Debug metadata drift by inspecting `controller_meta` serialization/load paths.

### Risks and Notes

- Raft log/hardstate durability is limited by `MemStorage`; metadata snapshots persist, but full consensus replay durability is not equivalent to disk-backed Raft storage.
- Controller node combines transport IO, consensus, and metadata apply in one loop, so overload can affect proposal latency.

Changes:

- Replace `MemStorage` with durable Raft log/hardstate storage.
  > Blocked: controller consensus still uses `RawNode<MemStorage>` in [`src/broker/controller/raft_node.rs`](../src/broker/controller/raft_node.rs). Replacing this requires selecting and integrating a disk-backed `raft::Storage` implementation (for example a custom sled-backed log/hardstate store) and migrating recovery paths in `handle_ready`. Current code only persists metadata snapshots/peers, not consensus log/hardstate.
- Define snapshot/compaction policy and startup recovery guarantees.
  > Blocked: snapshot/compaction policy cannot be finalized while Raft still runs on `MemStorage`; once durable Raft storage is introduced, policy choices must define thresholds, snapshot trigger points, retained log window, and restore guarantees across restarts in [`src/broker/controller/raft_node.rs`](../src/broker/controller/raft_node.rs).
