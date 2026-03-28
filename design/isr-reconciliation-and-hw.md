## ISR Reconciliation and HW Advancement

### Purpose

Track replica fetch progress for leader-owned partitions, recompute ISR membership, and advance high-watermark based on the slowest in-sync replica.

### Scope

**In scope:**
- `IsrManager` in [`src/broker/kraft/isr_manager.rs`](../src/broker/kraft/isr_manager.rs).
- Leader transition hook `on_became_leader` and progress ingestion API `record_replica_fetch` in [`src/broker/kraft/broker.rs`](../src/broker/kraft/broker.rs).
- ISR tick loop in `run_kraft_cluster` in [`src/main.rs`](../src/main.rs).

**Out of scope:**
- Actual fetch transport implementation for followers.
- Controller consensus transport details.

### Primary User Flow

1. Partition leadership moves to this broker.
2. Broker registers partition in ISR manager with replica set and initial ISR.
3. Periodic tick recalculates ISR and HW from observed replica progress.
4. Broker proposes partition metadata changes to controller when ISR/HW-related changes occur.

### System Flow

1. `on_became_leader(topic, partition, isr)` opens partition log and calls `isr_mgr.add_partition(...)`.
2. Optional progress updates are fed via `record_replica_fetch(topic, partition, replica_id, fetch_offset)`.
3. Tick task (500ms) in `main.rs` calls `isr_mgr.tick()`.
4. `tick()`:
- Refresh leader progress from local LEO.
- Build new ISR using lag (`lag_max`) and staleness (`stale_ms`) filters.
- Compute new HW = min(fetch_offset across ISR members).
- Advance log HW if larger.
- Emit `IsrChange` when ISR or HW changes.
5. Runtime proposes `PartitionChange` commands to controller using emitted `IsrChange`.

### Data Model

- `IsrChange { topic, partition, new_isr: Vec<u64>, new_hw: i64 }`.
- `ReplicaProgress { fetch_offset: i64, last_contact_ms: i64 }`.
- `PartitionIsrState { all_replicas: Vec<u64>, progress: HashMap<u64, ReplicaProgress>, current_isr: Vec<u64> }`.
- `IsrManager` params: `this_node`, `lag_max`, `stale_ms`.

Persistence behavior:
- ISR manager state is in-memory.
- Resulting HW updates persist through `PartitionLog`.
- ISR membership persistence depends on successful controller partition-change proposals.

### Interfaces and Contracts

- Public APIs:
- `add_partition`, `remove_partition`, `record_fetch_progress`, `tick`.
- Tick output contract: caller should propose controller metadata changes for each returned `IsrChange`.

### Dependencies

**Internal modules:**
- `PartitionLog` for LEO/HW reads/writes.
- Controller handle for partition change proposals.

**External services/libraries:**
- `tokio::sync::Mutex` for async state synchronization.

### Failure Modes and Edge Cases

- Missing log for tracked key causes tick to skip that partition.
- `advance_hw` failures are logged and do not crash tick loop.
- Replicas with no recent contact are removed from ISR even if historically in ISR.

### Observability and Debugging

- Logs on HW advance failures and ISR proposal failures.
- Useful debug pivots:
- Current `progress` per replica.
- `lag_max` and `stale_ms` thresholds.
- Emitted `IsrChange` list each tick.

### Risks and Notes

- Current fetch handler path does not consume `FetchRequest.replica_id` to feed `record_replica_fetch`, so ISR accuracy depends on other wiring.
- ISR changes are eventually consistent with controller metadata based on proposal success.

Changes:

- Wire `FetchRequest.replica_id` progress updates into `IsrManager::record_fetch_progress`.
- Add debounce/rate-limits for ISR change proposals to reduce control-plane churn.
- Persist/restore ISR tracking inputs where needed to reduce restart-time ISR oscillation.
