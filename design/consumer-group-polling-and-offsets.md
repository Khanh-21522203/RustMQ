## Consumer Group Polling and Offsets

### Purpose

Run consumer-side polling loops with optional group membership, partition assignment updates, heartbeat, offset tracking, and commit operations.

### Scope

**In scope:**
- Consumer runtime in [`src/client/consumer.rs`](../src/client/consumer.rs).
- Group protocol calls through `KafkaBrokerClient`.

**Out of scope:**
- Broker-side group coordinator logic.
- Producer routing behavior.

### Primary User Flow

1. Caller creates `Consumer::new` with `ConsumerConfig`.
2. Caller starts background mode using `start(handler)` or manual mode via `poll()`.
3. Consumer joins group (if configured), fetches messages, and updates offsets.
4. Consumer commits offsets periodically or manually.
5. On shutdown, consumer sends `LeaveGroup` and stops background tasks.

### System Flow

1. `start(handler)`:
- Guard against duplicate start.
- Join/sync group (`join_and_sync`) if `group_id` exists.
- Resolve initial offsets per active partition (`resolve_starting_offsets`).
- Spawn heartbeat task and poll/commit loop task.
2. Heartbeat task every 10s sends `HeartbeatRequest` with current `(member_id, generation_id)` from shared group session state.
3. Poll loop:
- If rebalance flag set, rejoin and refresh active partitions.
- Else fetch all active partitions (`fetch_all_partitions`) capped by `max_messages_per_poll`.
- Call handler for each message and advance in-memory next offsets (`offset + 1`) only after successful handler completion.
4. Auto-commit loop commits all current offsets when enabled.
5. `shutdown` signals tasks, optionally commits, and sends `LeaveGroup`.

### Data Model

- `ConsumedMessage { topic, partition, offset, key, value, timestamp }`.
- `Consumer` key fields:
- `partition_offsets: Arc<Mutex<HashMap<i32, i64>>>`,
- `active_partitions: Vec<i32>`,
- `needs_rejoin: Arc<AtomicBool>`,
- `member_id: Option<String>`.
- `MessageHandler` trait:
- `async fn handle(&self, message: ConsumedMessage) -> Result<()>`.

Persistence behavior:
- Runtime offsets are in-memory snapshots.
- Durable offsets depend on broker commit RPCs.

### Interfaces and Contracts

- Public APIs:
- `new`, `start`, `poll`, `commit`, `current_offset`, `current_offsets`, `seek`, `shutdown`.
- Offset sentinel contract:
- `offset = -2` -> earliest, `-1` -> latest, `>= 0` -> explicit start offset.
- Group retry contract:
- `error_code = 14` (`REBALANCE_IN_PROGRESS`) treated as rebalance and retried in `join_and_sync`.
- Backpressure contract:
- `max_messages_per_poll` bounds records fetched/handled per poll cycle.

### Dependencies

**Internal modules:**
- `KafkaBrokerClientTrait` RPC calls (`join_group`, `sync_group`, `heartbeat`, `fetch`, `commit_offset`, `leave_group`, `list_offsets`, `fetch_offset`).

**External services/libraries:**
- `tokio` intervals, channels, sleeps.
- `bincode` decode for `member_assignment` payload.

### Failure Modes and Edge Cases

- `start` fails when consumer already running.
- Rejoin may fail transiently and is retried by loop.
- Mutex poison errors are converted to `anyhow` failures.
- Commit is skipped when no `group_id`.
- Manual `poll()` mode still advances offsets on returned messages because handler success is managed by caller code.

### Observability and Debugging

- Logs include group assignments, rejoin attempts, fetch/commit failures, and shutdown events.
- Debug stuck consumers by inspecting `needs_rejoin`, `active_partitions`, and current offsets.

### Risks and Notes

- Background handler mode is now at-least-once friendly within process lifetime by advancing offsets only on handler success.
- Manual and background modes share core helpers but have distinct timing behavior, so bugs can appear in one mode only.

Changes:

