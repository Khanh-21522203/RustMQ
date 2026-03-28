## Consumer Offset State

### Purpose

Store and retrieve committed consumer offsets per `(group, topic, partition)`.

### Scope

**In scope:**
- `handle_commit_offset` and `handle_fetch_offset` in [`src/broker/server/core.rs`](../src/broker/server/core.rs).
- `commit_offset` and `fetch_offset` storage trait methods in [`src/broker/storage/traits.rs`](../src/broker/storage/traits.rs) and KRaft broker override in [`src/broker/kraft/broker.rs`](../src/broker/kraft/broker.rs).

**Out of scope:**
- Group membership and rebalance protocol.
- Producer/fetch message data path.

### Primary User Flow

1. Consumer sends `CommitOffset` with group/topic/partition offsets.
2. Broker stores offsets and optional metadata.
3. Consumer restart or reassignment sends `FetchOffset`.
4. Broker returns last committed values or `-1` when none exists.

### System Flow

1. `handle_commit_offset` iterates topic/partition entries and calls `storage.commit_offset`.
2. `handle_fetch_offset` iterates requested partitions and calls `storage.fetch_offset`.
3. In-memory backend writes nested map entries.
4. KRaft backend writes to local `committed_offsets` map in `KRaftBroker`.

### Data Model

- RPC commit input:
- `OffsetCommitRequest { consumer_group_id, topics[].partitions[] }`.
- `OffsetCommitRequest.PartitionData { partition, offset, metadata }`.
- RPC fetch output:
- `OffsetFetchResponse.PartitionResult { partition, offset, metadata, error_code }`.
- In-memory storage shape:
- `HashMap<String, HashMap<String, HashMap<i32, (i64, String)>>>`.
- KRaft storage shape:
- `HashMap<(String, String, i32), (i64, String)>` keyed by `(group, topic, partition)`.

Persistence behavior:
- Current implementations keep committed offsets in memory for process lifetime only.

### Interfaces and Contracts

- RPC contracts: `CommitOffset` and `FetchOffset` in [`src/api/kafka.proto`](../src/api/kafka.proto).
- Missing offset contract: returns `offset = -1`, empty metadata, and `error_code = 0`.

### Dependencies

**Internal modules:**
- Broker core RPC handlers.
- Selected storage backend implementation.

**External services/libraries:**
- None directly.

### Failure Modes and Edge Cases

- Storage write/read errors map to `error_code = 1`.
- No bounds validation: committed offsets may be ahead/behind actual log range.
- No replication guarantees in current offset store implementations.

### Observability and Debugging

- Router logs group id for commit/fetch operations.
- Core logs commit/fetch failures.
- Debug committed values by inspecting storage maps in `storage/traits.rs` or `kraft/broker.rs`.

### Risks and Notes

- Offset durability is weaker than message durability in KRaft path because offsets are not in controller metadata log.
- Cross-node failover semantics for committed offsets are not guaranteed by current code.

Changes:

- Persist committed offsets durably (not process-local maps) and document failover semantics.
- Add offset commit validation (range and monotonic checks per `(group, topic, partition)`).
