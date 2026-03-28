## Follower Replication Fetch Loop

### Purpose

Provide reusable follower-side background fetch tasks that copy records from a partition leader into local partition logs.

### Scope

**In scope:**
- `ReplicationManager` and `ReplicaFetcher` contract in [`src/broker/kraft/replication_manager.rs`](../src/broker/kraft/replication_manager.rs).
- Fetch loop behavior (`fetch_loop`) and task lifecycle APIs.

**Out of scope:**
- Production network fetcher implementation details (currently trait-based).
- Controller metadata proposal pipeline.

### Primary User Flow

1. Runtime decides this broker is follower for a partition.
2. Runtime starts fetch task for `(topic, partition)` with a `ReplicaFetcher` implementation.
3. Task repeatedly fetches from leader, appends to local log, and advances follower HW.
4. Runtime stops/replaces task when leadership/assignment changes.

### System Flow

1. `ReplicationManager::start_fetch_task`:
- Abort existing task for key if present.
- Spawn `fetch_loop(topic, partition, log, fetcher)`.
2. `fetch_loop` cycle:
- Use local `log.log_end_offset()` as fetch offset.
- Call `fetcher.fetch(topic, partition, fetch_offset, MAX_FETCH_BYTES)`.
- Append returned records to local `PartitionLog`.
- Advance HW to `min(leader_hw, local_leo)`.
- Sleep short interval on empty result, longer backoff on errors.
3. `stop_fetch_task`, `update_leader`, and `stop_all` manage task cancellation.

### Data Model

- `FetchedRecord { key: Option<Vec<u8>>, value: Vec<u8> }`.
- `FetchResult { records: Vec<FetchedRecord>, leader_hw: i64 }`.
- Task map: `HashMap<(String, i32), JoinHandle<()>>`.

Persistence behavior:
- Records are persisted by underlying `PartitionLog` (sled-backed) once appended.

### Interfaces and Contracts

- `ReplicaFetcher` trait:
- `fetch(topic, partition, fetch_offset, max_bytes) -> anyhow::Result<FetchResult>`.
- `ReplicationManager` APIs:
- `start_fetch_task`, `stop_fetch_task`, `update_leader`, `stop_all`, `active_task_count`.

### Dependencies

**Internal modules:**
- `PartitionLog` for append/HW operations.

**External services/libraries:**
- `tokio` tasks, mutex, and sleep.

### Failure Modes and Edge Cases

- Append failures are logged and retried after backoff.
- Fetch errors are logged and retried after backoff.
- Starting a task for an existing key replaces prior task.

### Observability and Debugging

- Log messages include topic-partition context for append/fetch/HW issues.
- `active_task_count()` is a direct debugging signal for task lifecycle correctness.

### Risks and Notes

- No production `ReplicaFetcher` wiring is present in current runtime path, so this module is currently scaffolding plus tests.
- Without integration into assignment transitions, follower catch-up may not occur automatically.

Changes:

- Implement a production `ReplicaFetcher` and hook task start/stop to assignment and leadership transitions.
- Add per-task health metrics (`last_success_ms`, lag, retry count) and alertable logs.
- Add configurable backoff tuning for fetch-loop retries.
