# Feature: Core Types

## 1. Purpose

Core Types is the foundational module that defines every primitive type used across the entire Rust-MQ system. All other modules import from here. It contains no business logic — only type aliases, newtype wrappers, encoding helpers, and constants.

Without a shared types module, every subsystem would define its own `Topic`, `Partition`, or `Offset`, leading to incompatible types, implicit conversions, and coupling between crates. Defining them once here lets the Rust compiler enforce type safety across the entire codebase.

## 2. Responsibilities

- Define `Topic` (newtype over `String`): names the logical log a producer writes to and a consumer reads from
- Define `Partition` (`i32`): zero-based index identifying a partition within a topic
- Define `Offset` (`i64`): monotonically increasing position of a message within a partition
- Define `GroupId` (newtype over `String`): identifies a consumer group for coordinated offset management
- Define `NodeId` (`u64`): identifies a broker node within a Raft cluster
- Define `MessageKey` (`Option<Vec<u8>>`): optional routing key for a message
- Define `MessageValue` (`Vec<u8>`): opaque payload bytes
- Define `Message`: compound type combining key, value, and assigned offset
- Define special offset sentinel constants: `OFFSET_EARLIEST`, `OFFSET_LATEST`
- Define `ErrorCode`: Kafka-compatible numeric error codes returned in gRPC responses
- Define `BrokerAddress`: typed `String` for a `"host:port"` broker endpoint
- Provide `Display` / `FromStr` implementations for all public types

## 3. Non-Responsibilities

- No I/O or disk access
- No network calls
- No business logic (routing, replication, offset resolution decisions)
- No configuration loading
- No logging or metrics
- No serde derives unless the type is serialized on the wire (protobuf handles that)

## 4. Architecture Design

Core Types sits at the bottom of the dependency graph. Every other module depends on it; it depends on nothing beyond the Rust standard library.

```
+----------------------------------------------------------+
|                  All Rust-MQ Modules                     |
|  broker | client | raft | config | api | cli             |
+-----------------------------+----------------------------+
                              |
                      use rust_mq::types
                              |
              +---------------+---------------+
              |       rust_mq::types          |
              |  Topic      Partition  Offset |
              |  GroupId    NodeId     Message|
              |  ErrorCode  BrokerAddress     |
              |  OFFSET_EARLIEST/LATEST       |
              +-------------------------------+
```

**Offset sentinel values**:
- `-2` → `OFFSET_EARLIEST`: start from the first retained message in the partition
- `-1` → `OFFSET_LATEST`: start from the next message to be written (skip all existing)
- `0+` → exact offset: resume from a known position

**ErrorCode mapping** (mirrors Kafka error codes for protocol compatibility):

| Value | Name | Meaning |
|---|---|---|
| 0 | `NoError` | Success |
| 1 | `OffsetOutOfRange` | Requested offset does not exist |
| 3 | `UnknownTopicOrPartition` | Topic or partition not found |
| 5 | `LeaderNotAvailable` | No Raft leader elected |
| 6 | `NotLeaderForPartition` | Write rejected; node is not leader |
| 16 | `NotCoordinatorForGroup` | Node is not the group coordinator |
| 22 | `IllegalGeneration` | Consumer group generation mismatch |
| 25 | `UnknownMemberId` | Group member ID not recognized |
| 35 | `GroupLoadInProgress` | Group metadata loading |

## 5. Core Data Structures (Rust)

```rust
// src/types.rs

/// Names a logical message log. Producers write to topics; consumers read from them.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Topic(pub String);

impl Topic {
    pub fn new(s: impl Into<String>) -> Self { Topic(s.into()) }
    pub fn as_str(&self) -> &str { &self.0 }
}

/// Zero-based index identifying a partition within a topic.
pub type Partition = i32;

/// Monotonically increasing position of a message within a partition.
/// Assigned by the broker on write; never reused within a partition.
pub type Offset = i64;

/// Sentinel: start consuming from the first retained message.
pub const OFFSET_EARLIEST: Offset = -2;

/// Sentinel: start consuming from the next message to be written.
pub const OFFSET_LATEST: Offset = -1;

/// Identifies a consumer group. The broker stores committed offsets per group.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GroupId(pub String);

impl GroupId {
    pub fn new(s: impl Into<String>) -> Self { GroupId(s.into()) }
}

/// Identifies a broker node within a Raft cluster.
pub type NodeId = u64;

/// A message as stored in a topic partition.
#[derive(Debug, Clone)]
pub struct Message {
    /// Optional routing key. Two messages with the same key always go to the same partition.
    pub key: Option<Vec<u8>>,
    /// Opaque payload bytes. Serialization is the application's responsibility.
    pub value: Vec<u8>,
    /// Offset assigned by the broker. Zero until the broker writes the message.
    pub offset: Offset,
}

/// Kafka-compatible numeric error code returned in all gRPC responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ErrorCode {
    NoError                  = 0,
    OffsetOutOfRange         = 1,
    CorruptMessage           = 2,
    UnknownTopicOrPartition  = 3,
    LeaderNotAvailable       = 5,
    NotLeaderForPartition    = 6,
    NotCoordinatorForGroup   = 16,
    IllegalGeneration        = 22,
    UnknownMemberId          = 25,
    GroupLoadInProgress      = 35,
}

impl ErrorCode {
    pub fn is_error(self) -> bool { self != ErrorCode::NoError }
}

/// A typed broker endpoint address ("scheme://host:port" or "host:port").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerAddress(pub String);

impl BrokerAddress {
    pub fn new(s: impl Into<String>) -> Self { BrokerAddress(s.into()) }
}
```

## 6. Public Interfaces

All types in this module are exported. There are no trait objects — only concrete types and helper methods.

```rust
// Type constructors
Topic::new(s: impl Into<String>) -> Topic
GroupId::new(s: impl Into<String>) -> GroupId
BrokerAddress::new(s: impl Into<String>) -> BrokerAddress

// Type methods
Topic::as_str(&self) -> &str
GroupId::as_str(&self) -> &str   // mirrors Topic pattern
ErrorCode::is_error(self) -> bool

// Standard trait impls (all types)
impl Display for Topic / GroupId / BrokerAddress
impl FromStr for Topic / GroupId / BrokerAddress
impl From<String> for Topic / GroupId / BrokerAddress

// Constants
OFFSET_EARLIEST: Offset = -2
OFFSET_LATEST:   Offset = -1
```

## 7. Internal Algorithms

### Offset Resolution Order (used by Consumer, documented here for reference)

The consumer resolves its starting offset in this priority order:
1. If the broker has a committed offset for `(group_id, topic, partition)`: use that
2. Else if `config.offset == OFFSET_EARLIEST`: call `ListOffsets` and use earliest
3. Else if `config.offset == OFFSET_LATEST`: call `ListOffsets` and use latest
4. Else if `config.offset >= 0`: use the configured offset directly

This logic lives in the Consumer module; the types module only defines the sentinels.

### ErrorCode from i32

```
ErrorCode::from_i32(code: i32) -> Option<ErrorCode>:
  match code:
    0  -> Some(NoError)
    1  -> Some(OffsetOutOfRange)
    3  -> Some(UnknownTopicOrPartition)
    5  -> Some(LeaderNotAvailable)
    6  -> Some(NotLeaderForPartition)
    16 -> Some(NotCoordinatorForGroup)
    22 -> Some(IllegalGeneration)
    25 -> Some(UnknownMemberId)
    35 -> Some(GroupLoadInProgress)
    _  -> None
```

## 8. Persistence Model

Not applicable. Core types are not persisted directly — they appear embedded in other persisted structures (Raft log entries, protobuf messages, YAML config).

## 9. Concurrency Model

All types in this module are value types (`Clone` + `Send + Sync`). They are safe for concurrent use when cloned or passed by value. No locks, channels, or async tasks.

## 10. Configuration

No configuration. All constants are compile-time:

```rust
pub const OFFSET_EARLIEST: Offset = -2;
pub const OFFSET_LATEST: Offset = -1;
pub const DEFAULT_API_PORT: u16 = 9092;
pub const DEFAULT_RPC_PORT: u16 = 19092;
```

## 11. Observability

No metrics or logging in this module. Errors are returned as typed `anyhow::Error` or `thiserror`-derived types for callers to log.

## 12. Testing Strategy

**Unit tests** (table-driven):
- `test_topic_roundtrip`: `Topic::new("foo").as_str() == "foo"`, `Display` matches inner string
- `test_group_id_equality`: two `GroupId`s with same string are equal; different strings are not
- `test_error_code_is_error`: `NoError.is_error() == false`; all others return `true`
- `test_error_code_from_i32`: valid codes map correctly; unknown values return `None`
- `test_offset_sentinels`: `OFFSET_EARLIEST == -2`, `OFFSET_LATEST == -1`
- `test_broker_address_display`: round-trip through `FromStr` and `Display`

All tests use only the standard library `core` and `std`. No external dependencies.

## 13. Open Questions

None.
