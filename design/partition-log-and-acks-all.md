## Partition Log and Acks-All Semantics

### Purpose

Provide per-partition durable append/read storage with high-watermark gating and implement `acks=all` behavior for produce operations.

### Scope

**In scope:**
- `PartitionLog` in [`src/broker/kraft/partition_log.rs`](../src/broker/kraft/partition_log.rs).
- Produce/fetch/offset methods in `KRaftBroker` (`produce_message`, `produce_message_acks_all`, `fetch_messages`, `get_partition_offset`) in [`src/broker/kraft/broker.rs`](../src/broker/kraft/broker.rs).

**Out of scope:**
- Controller metadata replication.
- Group coordination and offset commit maps.

### Primary User Flow

1. Producer writes records to partition leader.
2. Leader appends to local `PartitionLog` and returns offset.
3. For `acks=all`, producer waits until partition HW advances past written offset.
4. Consumers fetch only messages below HW.

### System Flow

1. `KRaftBroker::produce_message` validates leader ownership from controller metadata.
2. Leader appends record via `PartitionLog::append`.
3. Single-replica fast path advances HW immediately to `offset + 1`.
4. `produce_message_acks_all`:
- Calls `produce_message`.
- Subscribes to `PartitionLog` HW watch channel.
- Waits up to 5 seconds for HW >= target.
5. `fetch_messages` reads via `PartitionLog::read(start_offset, max_bytes)` which enforces `< HW` visibility.

### Data Model

- `LogEntry { offset: i64, key: Option<Vec<u8>>, value: Vec<u8>, timestamp_ms: i64 }`.
- `PartitionLog` fields:
- `tree: sled::Tree`
- `leo: AtomicI64` (next write offset)
- `hw: AtomicI64` (next committed offset)
- `hw_tx: watch::Sender<i64>`

Persistence behavior:
- Entries stored in sled with 8-byte big-endian offset keys.
- HW persisted under `__hw__` key.
- `open()` restores LEO and HW from sled.

### Interfaces and Contracts

- Storage trait contract via `BrokerStorage` implementation in `KRaftBroker`.
- Offset sentinel contract in `get_partition_offset`:
- `time = -1` -> latest (LEO).
- `time = -2` -> earliest.

### Dependencies

**Internal modules:**
- Controller metadata lookups for leader checks.
- ISR manager for HW advancement beyond single-replica fast path.

**External services/libraries:**
- `sled` for persistence.
- `tokio::sync::watch` for HW notifications.

### Failure Modes and Edge Cases

- Produce to non-leader returns `BrokerError::NotLeader { leader_addr }`.
- Partition with leader `0` returns internal error.
- `acks=all` wait times out after 5 seconds with internal timeout error.
- Missing partition log in `acks=all` wait path returns success (defensive fallback).

### Observability and Debugging

- Core layer logs produce/fetch errors.
- Partition log debugging starts at `log_end_offset()`, `high_watermark()`, and persisted tree contents.
- `watch` channel updates are the source of `acks=all` unblock behavior.

### Risks and Notes

- HW used in client-facing fetch response from core is not sourced directly from partition log HW in all paths.
- `PartitionLog::truncate_before` leaves LEO/HW management to caller; misuse can create inconsistent visibility if not coordinated.

Changes:

