# Feature: Broker Storage

## 1. Purpose

Broker Storage defines the `BrokerStorage` trait — the single interface through which the entire broker interacts with its state. It provides a clean seam between the request-handling layer and the underlying persistence mechanism, allowing the same `BrokerCore` to work in both single-node (in-memory) and multi-node (Raft-replicated) deployments without any code changes.

`InMemoryStorage` is the reference implementation: a simple `HashMap`-based store that holds all state in process memory. It is authoritative for single-broker development and testing deployments. The `MultiBroker` implementation (see §plan-multi-broker) wraps a Raft log and implements the same trait for high-availability clusters.

## 2. Responsibilities

- Define the `BrokerStorage` async trait with all methods required by `BrokerCore`
- Implement `InMemoryStorage`: `HashMap`-backed, in-process, no persistence
- Store messages per `(topic, partition)` as an ordered `Vec<Message>`, indexable by offset
- Store committed consumer offsets per `(group_id, topic, partition)`
- Store consumer group membership state: members, generation, assignments
- Expose earliest and latest offset queries for a given `(topic, partition)`
- Expose topic metadata: number of partitions, leader node
- Protect all mutable state with `Arc<RwLock<T>>` for safe concurrent access from Tokio tasks

## 3. Non-Responsibilities

- Does not handle gRPC serialization or deserialization
- Does not perform Raft consensus (that is `MultiBroker`'s role)
- Does not persist state to disk (`InMemoryStorage` is volatile by design)
- Does not assign offsets (offsets are determined by position in the Vec)
- Does not replicate writes to other nodes
- Does not enforce topic creation policies or partition count limits

## 4. Architecture Design

```
BrokerCore<S: BrokerStorage>
       |
       | calls trait methods
       v
+------+------------------------------------+
|         BrokerStorage trait               |
|  produce_message()    fetch_messages()    |
|  list_offsets()       get_topic_metadata()|
|  commit_offset()      fetch_offset()      |
|  join_group()         sync_group()        |
|  heartbeat()          leave_group()       |
+------+--------------------+---------------+
       |                    |
       v                    v
InMemoryStorage         MultiBroker
(single-node,           (Raft-backed,
 HashMap, volatile)      see plan-multi-broker)

InMemoryStorage internal layout:
+-------------------------------------------+
| messages: HashMap<(Topic, Partition),     |
|             Vec<Message>>                 |
|                                           |
| offsets:  HashMap<(GroupId, Topic,        |
|             Partition), u64>              |
|                                           |
| groups:   HashMap<GroupId, GroupState>    |
+-------------------------------------------+
```

**Offset assignment**: The offset of a message is its zero-based index in the partition's `Vec`. When a new message is appended, `offset = vec.len() as i64` before the push.

## 5. Core Data Structures (Rust)

```rust
// src/broker/storage.rs

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// The complete interface that all broker backends must implement.
/// All methods are async and Send-safe so they can be called from Tokio tasks.
#[async_trait]
pub trait BrokerStorage: Send + Sync + 'static {

    // ── Message operations ──────────────────────────────────────────────────

    /// Append a message to `(topic, partition)`. Returns the assigned offset.
    async fn produce_message(
        &self,
        topic: &str,
        partition: i32,
        key: Option<Vec<u8>>,
        value: Vec<u8>,
    ) -> anyhow::Result<i64>;

    /// Fetch up to `max_bytes` bytes worth of messages from `(topic, partition)`
    /// starting at `offset`. Returns an empty Vec if no messages are available
    /// at that offset (not an error).
    async fn fetch_messages(
        &self,
        topic: &str,
        partition: i32,
        offset: i64,
        max_bytes: i32,
    ) -> anyhow::Result<Vec<StoredMessage>>;

    // ── Offset queries ──────────────────────────────────────────────────────

    /// Return the earliest available offset for `(topic, partition)`.
    /// Returns 0 if the partition is empty.
    async fn earliest_offset(&self, topic: &str, partition: i32) -> anyhow::Result<i64>;

    /// Return the next offset to be written for `(topic, partition)`.
    /// Equivalent to `message_count` in the partition.
    async fn latest_offset(&self, topic: &str, partition: i32) -> anyhow::Result<i64>;

    // ── Metadata ────────────────────────────────────────────────────────────

    /// Return partition count and leader info for the requested topics.
    /// Unknown topics return an error entry, not an error from this method.
    async fn get_topic_metadata(
        &self,
        topics: &[String],
    ) -> anyhow::Result<Vec<TopicMetadata>>;

    // ── Consumer group offset management ───────────────────────────────────

    /// Persist the consumer's current position. Overwrites any previous value.
    async fn commit_offset(
        &self,
        group_id: &str,
        topic: &str,
        partition: i32,
        offset: i64,
    ) -> anyhow::Result<()>;

    /// Retrieve the last committed offset for `(group_id, topic, partition)`.
    /// Returns `None` if no offset has been committed.
    async fn fetch_offset(
        &self,
        group_id: &str,
        topic: &str,
        partition: i32,
    ) -> anyhow::Result<Option<i64>>;

    // ── Consumer group membership ──────────────────────────────────────────

    async fn join_group(
        &self,
        group_id: &str,
        member_id: &str,
        topics: Vec<String>,
    ) -> anyhow::Result<JoinGroupResult>;

    async fn sync_group(
        &self,
        group_id: &str,
        member_id: &str,
        generation_id: i32,
        assignments: Vec<MemberAssignment>,
    ) -> anyhow::Result<Vec<u8>>;

    async fn heartbeat(
        &self,
        group_id: &str,
        member_id: &str,
        generation_id: i32,
    ) -> anyhow::Result<()>;

    async fn leave_group(
        &self,
        group_id: &str,
        member_id: &str,
    ) -> anyhow::Result<()>;
}

// ── Supporting types ────────────────────────────────────────────────────────

/// A message as returned by `fetch_messages`.
#[derive(Debug, Clone)]
pub struct StoredMessage {
    pub offset: i64,
    pub key: Option<Vec<u8>>,
    pub value: Vec<u8>,
}

/// Metadata for a single topic returned by `get_topic_metadata`.
#[derive(Debug, Clone)]
pub struct TopicMetadata {
    pub name: String,
    pub error_code: i32,
    pub partitions: Vec<PartitionMetadata>,
}

#[derive(Debug, Clone)]
pub struct PartitionMetadata {
    pub index: i32,
    pub error_code: i32,
    pub leader_id: i32,
}

#[derive(Debug, Clone)]
pub struct JoinGroupResult {
    pub generation_id: i32,
    pub member_id: String,
    pub is_leader: bool,
    pub members: Vec<GroupMember>,
}

#[derive(Debug, Clone)]
pub struct GroupMember {
    pub member_id: String,
    pub subscriptions: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct MemberAssignment {
    pub member_id: String,
    pub assignment: Vec<u8>,
}

// ── InMemoryStorage ─────────────────────────────────────────────────────────

/// Single-node, volatile, HashMap-based BrokerStorage implementation.
/// All state is lost when the process exits.
pub struct InMemoryStorage {
    inner: Arc<RwLock<InMemoryState>>,
}

struct InMemoryState {
    /// (topic, partition) → ordered message log; index == offset
    messages: HashMap<(String, i32), Vec<StoredMessage>>,
    /// (group_id, topic, partition) → committed offset
    offsets: HashMap<(String, String, i32), i64>,
    /// group_id → group membership state
    groups: HashMap<String, GroupState>,
}

struct GroupState {
    generation_id: i32,
    leader_member_id: Option<String>,
    members: HashMap<String, GroupMember>,
    assignments: HashMap<String, Vec<u8>>,
}
```

## 6. Public Interfaces

```rust
// Trait (see §5)
pub trait BrokerStorage: Send + Sync + 'static { ... }

// InMemoryStorage
impl InMemoryStorage {
    /// Create a new empty in-memory store.
    pub fn new() -> Self;
}

impl BrokerStorage for InMemoryStorage { ... }
```

## 7. Internal Algorithms

### produce_message
```
produce_message(topic, partition, key, value):
  state = inner.write().await
  log = state.messages
           .entry((topic.to_owned(), partition))
           .or_insert_with(Vec::new)
  offset = log.len() as i64
  log.push(StoredMessage { offset, key, value })
  return Ok(offset)
```

### fetch_messages
```
fetch_messages(topic, partition, offset, max_bytes):
  state = inner.read().await
  log = state.messages.get(&(topic, partition))
        → if None: return Ok(vec![])
  if offset < 0 or offset >= log.len():
    return Ok(vec![])
  result = []
  total_bytes = 0
  for msg in log[offset..]:
    msg_bytes = msg.value.len() + msg.key.map_or(0, |k| k.len())
    if total_bytes + msg_bytes > max_bytes and !result.is_empty():
      break
    result.push(msg.clone())
    total_bytes += msg_bytes
  return Ok(result)
```

### commit_offset / fetch_offset
```
commit_offset(group_id, topic, partition, offset):
  state = inner.write().await
  state.offsets.insert((group_id, topic, partition), offset)
  return Ok(())

fetch_offset(group_id, topic, partition):
  state = inner.read().await
  return Ok(state.offsets.get(&(group_id, topic, partition)).copied())
```

### join_group
```
join_group(group_id, member_id, topics):
  state = inner.write().await
  group = state.groups.entry(group_id).or_insert(GroupState::new())
  group.generation_id += 1
  is_leader = group.members.is_empty()
  if is_leader: group.leader_member_id = Some(member_id)
  group.members.insert(member_id, GroupMember { member_id, subscriptions: topics })
  return Ok(JoinGroupResult {
    generation_id: group.generation_id,
    member_id,
    is_leader,
    members: group.members.values().cloned().collect(),
  })
```

### heartbeat
```
heartbeat(group_id, member_id, generation_id):
  state = inner.read().await
  group = state.groups.get(group_id) → if None: return Err(UnknownGroupId)
  if group.generation_id != generation_id: return Err(IllegalGeneration)
  if !group.members.contains_key(member_id): return Err(UnknownMemberId)
  return Ok(())
```

## 8. Persistence Model

`InMemoryStorage` is intentionally volatile. All state exists only in process memory and is lost on process exit. There is no WAL, no snapshot, and no recovery path.

For persistence in cluster mode, `MultiBroker` routes writes through the Raft log, which is persisted to `storage_path` on disk (see §plan-multi-broker and §plan-raft-consensus).

## 9. Concurrency Model

| Object | Primitive | Usage |
|---|---|---|
| `InMemoryState` | `Arc<RwLock<InMemoryState>>` | `RwLock::read()` for reads; `RwLock::write()` for writes and group mutations |

All `BrokerStorage` methods are `async`. Callers are Tokio tasks spawned by `BrokerCore`. The `RwLock` is `tokio::sync::RwLock` (async-aware, never blocks the executor).

**Deadlock avoidance**: The lock is never held across an `.await` boundary. Every method acquires the lock, performs its operation, releases the lock, then returns.

## 10. Configuration

`InMemoryStorage` has no configuration. It is constructed with `InMemoryStorage::new()` and requires no parameters.

## 11. Observability

- Messages produced: log at `TRACE` level with `(topic, partition, offset)`
- Fetch calls with empty result: log at `TRACE` (common during idle polling, not an error)
- Group membership changes: log at `DEBUG` with `(group_id, member_id, action)`
- All errors: log at `WARN` with context before returning

## 12. Testing Strategy

**Unit tests**:
- `test_produce_increments_offset`: produce 3 messages, assert offsets are 0, 1, 2
- `test_fetch_from_offset`: produce 5 messages, fetch from offset 2, assert messages 2–4 returned
- `test_fetch_respects_max_bytes`: produce large messages, assert fetch stops before exceeding limit
- `test_fetch_empty_partition`: fetch from unknown topic, assert empty Vec (not error)
- `test_offset_out_of_range`: fetch from offset beyond latest, assert empty Vec
- `test_commit_and_fetch_offset`: commit offset 42, fetch it back, assert equals 42
- `test_fetch_offset_no_commit`: fetch without prior commit, assert `None`
- `test_join_group_assigns_leader`: first member to join is the leader
- `test_join_group_increments_generation`: each join bumps generation_id
- `test_heartbeat_wrong_generation`: heartbeat with stale generation_id returns `IllegalGeneration`
- `test_leave_group_removes_member`: leave_group removes member from group state
- `test_concurrent_produce`: 10 Tokio tasks produce to same partition, assert no duplicate offsets

## 13. Open Questions

None.
