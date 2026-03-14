# Core Concepts

Rust-MQ is modeled after Apache Kafka's design. If you are already familiar with Kafka, most concepts map directly. This document explains the core abstractions from first principles.

## Topics

A **topic** is a named, ordered log of messages. Producers write to topics; consumers read from them. Topics are identified by a string name (e.g., `"events"`, `"orders"`).

Unlike a queue that deletes messages after delivery, a topic retains messages and allows any number of consumers to read from any position in the log — independently.

## Partitions

Each topic is divided into one or more **partitions**. A partition is the fundamental unit of ordering and parallelism:

- Messages within a single partition are strictly ordered by offset.
- Different partitions have no ordering guarantee relative to each other.
- Multiple consumers can read different partitions in parallel, enabling horizontal scaling.

Each partition is identified by a zero-based integer index (0, 1, 2, …).

```
Topic "orders"
├── Partition 0: [msg@0] [msg@1] [msg@2] ...
├── Partition 1: [msg@0] [msg@1] ...
└── Partition 2: [msg@0] [msg@1] [msg@2] [msg@3] ...
```

## Messages

A **message** (also called a record) is the unit of data written to a topic. Each message has:

- **Key** (optional): Used for routing — messages with the same key always go to the same partition.
- **Value**: The payload bytes. Rust-MQ treats this as opaque bytes; serialization is the application's responsibility.
- **Offset**: Assigned by the broker upon write. Monotonically increasing within a partition.

## Offsets

An **offset** is a 64-bit integer that uniquely identifies a message's position within a partition. Offsets are:

- Assigned by the broker, starting at 0.
- Monotonically increasing — never recycled within a partition.
- Used by consumers to track their progress and resume after restarts.

### Special Offset Values

When configuring a consumer's starting position, two sentinel values are recognized:

| Value | Meaning |
|---|---|
| `-2` | Earliest — start from the first available message |
| `-1` | Latest — start from the next message to be written (skip existing) |
| `0+` | Specific offset — resume from an exact position |

## Producers

A **producer** publishes messages to a topic and partition. Key producer behaviors:

- **Batching**: Messages are accumulated in a local buffer and sent as a batch to reduce network overhead and improve throughput.
- **Acknowledgments**: Producers can configure how many broker replicas must confirm a write before it is considered successful (`required_acks`).
- **Flush interval**: Batches are flushed either when they reach a size limit or after a configurable time interval.

## Consumers

A **consumer** reads messages from a topic partition. Key consumer behaviors:

- **Polling**: Consumers periodically fetch messages from the broker in batches.
- **Offset tracking**: Each consumer tracks which offset it has processed up to.
- **Auto-commit**: Offsets can be committed automatically on an interval, or manually for at-least-once / exactly-once semantics.

## Consumer Groups

A **consumer group** is a named set of consumers that together consume a topic. The broker tracks the committed offset for each `(group_id, topic, partition)` triple, allowing consumers to:

- Resume after restart without re-reading messages.
- Coordinate with other consumers in the same group.

Each consumer identifies itself to the broker with a `group_id`. When a consumer commits an offset, it is stored under that group ID.

## Brokers

A **broker** is the server that stores messages and serves producer/consumer requests. Rust-MQ supports two deployment modes:

- **Single broker**: One server with in-memory storage. Suitable for development and testing.
- **Broker cluster**: Three or more servers using Raft consensus for fault-tolerant, replicated storage.

In a cluster, one node is elected **leader** at any time. Only the leader accepts writes. Reads can be served by any node depending on consistency requirements.

## Raft Consensus

In a multi-broker deployment, Rust-MQ uses the **Raft** consensus algorithm to replicate state across nodes. Raft guarantees:

- A write is only acknowledged after a majority of nodes have persisted it.
- Leader election is automatic if the current leader becomes unavailable.
- The cluster can tolerate up to `(N-1)/2` node failures in an N-node cluster (e.g., 1 failure in a 3-node cluster).

See [Cluster Deployment](./deployment/cluster.md) for setup instructions.
