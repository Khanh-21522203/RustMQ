## Controller Heartbeat and Failure Detection

### Purpose

Track broker liveness through periodic heartbeat proposals and trigger partition failover assignments when brokers are considered dead.

### Scope

**In scope:**
- Heartbeat and detector tasks in `run_kraft_cluster` in [`src/main.rs`](../src/main.rs).
- Metadata updates for `BrokerHeartbeat` and `MarkBrokerDead` commands in [`src/broker/controller/state_machine.rs`](../src/broker/controller/state_machine.rs).
- Assignment structure in [`src/broker/controller/types.rs`](../src/broker/controller/types.rs).

**Out of scope:**
- Transport-level health checks.
- Actual follower data catch-up procedures.

### Primary User Flow

1. Each running broker periodically proposes its own heartbeat timestamp.
2. Active controller periodically scans broker `last_seen_ms` values.
3. If a broker exceeds dead threshold, controller computes new assignments and proposes `MarkBrokerDead`.
4. Metadata reflects removed broker and updated partition leaders/ISR.

### System Flow

1. Heartbeat loop (every 3s):
- Capture wall-clock ms.
- Call `propose_broker_heartbeat(node_id, timestamp_ms)`.
- Ignore/warn on non-`NOT_LEADER` errors.
2. Failure detector loop (every 5s, controller-only):
- Load metadata snapshot.
- For each broker with `last_seen_ms > 0`, check `now - last_seen_ms > 15000`.
- Compute failover using `compute_failover_assignments(meta, dead_broker_id)`.
- Propose `MarkBrokerDead` with computed assignments.
3. State machine apply:
- Remove dead broker from broker map.
- Update partition leader/ISR/replicas per assignment.

### Data Model

- `BrokerRegistration { broker_id, api_addr, rpc_addr, last_seen_ms }`.
- `ControllerCommand::BrokerHeartbeat { broker_id, timestamp_ms }`.
- `ControllerCommand::MarkBrokerDead { broker_id, new_assignments }`.
- `PartitionAssignment { topic, partition, new_leader, new_isr, replicas }`.

Persistence behavior:
- Heartbeat timestamps and dead-broker reassignment metadata persist through controller metadata persistence.

### Interfaces and Contracts

- Heartbeat update is monotonic in state machine (`timestamp_ms` only advances `last_seen_ms`).
- Failover selection contract in `compute_failover_assignments`:
- New leader = first alive ISR member.
- If no alive ISR members, leader becomes `0` (offline partition).

### Dependencies

**Internal modules:**
- `ControllerHandle` implementation.
- `compute_failover_assignments` helper.

**External services/libraries:**
- `tokio::time::interval` for periodic tasks.

### Failure Modes and Edge Cases

- Heartbeats proposed on follower nodes return `NOT_LEADER` by design.
- Brokers that never heartbeat (`last_seen_ms == 0`) are skipped by dead check grace rule.
- Repeated detector runs may reattempt dead proposals if metadata transitions lag.

### Observability and Debugging

- Warn logs: suspected dead broker and elapsed ms.
- Error logs: `propose_mark_broker_dead` failures.
- Debug heartbeat by tracing 3s proposals and metadata `last_seen_ms` updates.

### Risks and Notes

- Fixed dead threshold (15s) is hardcoded in `main.rs` task.
- Liveness detection is clock-based and local; no quorum-based health check in this layer.

Changes:

- Move heartbeat/dead-broker thresholds and detector intervals into broker config.
- Require consecutive missed-heartbeat windows before `MarkBrokerDead`.
- Add guardrails to prevent repeated dead-node proposals for the same broker within short windows.
