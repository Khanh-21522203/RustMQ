# Architecture

This document describes Rust-MQ's system design: its components, how they interact, and the design decisions behind them.

## Overview

Rust-MQ is structured as a library with a thin CLI wrapper. The core components are:

```
┌─────────────────────────────────────────────────┐
│                  Client Layer                   │
│                                                 │
│  Producer            Consumer                   │
│  - Batching          - Poll loop                │
│  - Flush interval    - Offset tracking          │
│  - Acks              - Auto-commit              │
└────────────┬──────────────────┬─────────────────┘
             │ gRPC (Tonic)     │
             ▼                  ▼
┌─────────────────────────────────────────────────┐
│              KafkaBrokerServer                  │
│           (Tonic gRPC endpoints)                │
└────────────────────┬────────────────────────────┘
                     │ mpsc channel
                     ▼
┌─────────────────────────────────────────────────┐
│              BrokerCore<S>                      │
│         (Generic request dispatcher)            │
└────────────────────┬────────────────────────────┘
                     │ BrokerStorage trait
          ┌──────────┴──────────┐
          ▼                     ▼
┌──────────────────┐  ┌─────────────────────────┐
│ InMemoryStorage  │  │     MultiBroker          │
│ (single-node)    │  │  (Raft-backed cluster)   │
└──────────────────┘  └──────────┬──────────────┘
                                 │
                                 ▼
                      ┌─────────────────────────┐
                      │    SimpleRaftNode        │
                      │  (raft crate RawNode)    │
                      │                         │
                      │  BrokerData             │
                      │  (replicated state)     │
                      └─────────────────────────┘
```

## Components

### Client Layer

**Producer** (`src/client/producer.rs`)

The producer accumulates messages in an in-memory batch. Batches are flushed to the broker either when the batch reaches a configured size or after a configurable time interval. This amortizes gRPC round-trip overhead across many messages, significantly improving throughput.

Producers expose two send modes:
- `send()` — adds to the batch; returns after the message is buffered
- `send_sync()` — flushes immediately and waits for broker acknowledgment

**Consumer** (`src/client/consumer.rs`)

The consumer runs a background poll loop that periodically fetches message batches from the broker. Each received message is passed to a user-supplied `MessageHandler` implementation. Offsets are optionally committed back to the broker on a configurable interval (auto-commit) or explicitly via `commit()`.

On startup, the consumer resolves its starting offset in order:
1. Committed offset for this group/topic/partition (resume semantics)
2. Offset policy from config (`-2` = earliest, `-1` = latest)
3. Defaults to 0

### gRPC Transport

**KafkaBrokerServer** (`src/broker/kafka_broker_server.rs`)

The gRPC server implements the `Broker` service defined in `kafka.proto`. It is built on [Tonic](https://github.com/hyperium/tonic) and accepts connections on the configured `api_addr`.

Each incoming RPC is forwarded to `BrokerCore` via an mpsc channel paired with a one-shot response channel. This decouples the gRPC handler (which must be `Send + 'static`) from the storage implementation.

**KafkaBrokerClient** (`src/client/kafka_broker_client.rs`)

A thin async wrapper around the generated Tonic client stub. It exposes the same 11-method interface as the server and is used by both `Producer` and `Consumer`.

### Broker Core

**BrokerCore<S>** (`src/broker/core.rs`)

`BrokerCore` is parameterized on a `BrokerStorage` implementation. It owns the mpsc receiver and dispatches incoming `BrokerGrpcRequest` variants to typed handler methods. All business logic for validating requests and constructing responses lives here, keeping it independent of both the transport layer and the storage backend.

### Storage Abstraction

**BrokerStorage trait** (`src/broker/storage.rs`)

The storage trait defines the interface all backends must implement:

- Message operations: `produce_message`, `fetch_messages`
- Offset queries: `list_offsets` (earliest, latest, specific)
- Consumer group operations: `join_group`, `sync_group`, `heartbeat`, `leave_group`
- Offset management: `commit_offset`, `fetch_offset`
- Metadata: `get_topic_metadata`

This trait enables the same `BrokerCore` to work in both single-node and cluster deployments.

**InMemoryStorage**

A `HashMap`-based implementation for single-node deployments. All state is held in memory; data does not survive process restarts. Suitable for development, testing, and ephemeral workloads.

**MultiBroker** (`src/broker/multi_broker.rs`)

Wraps the Raft storage layer and implements `BrokerStorage`. Produces and offset commits are routed through Raft consensus before being applied to the state machine. If the current node is not the Raft leader, write operations return an error; clients should retry against the leader.

### Raft Consensus Layer

**RaftNode** (`src/broker/raft.rs`)

Wraps the [`raft`](https://crates.io/crates/raft) crate's `RawNode`. Manages the Raft tick loop (100ms interval), leadership election, log replication, and state machine application.

The replicated state machine (`BrokerData`) holds:
- `messages`: `HashMap<(topic, partition), Vec<Message>>`
- `offsets`: `HashMap<(group_id, topic, partition), u64>`

Commands are serialized with `bincode` before being proposed to the Raft log.

**RaftNetwork** (`src/broker/raft_network.rs`)

Handles inter-node communication. Each node exposes a separate `rpc_addr` for Raft peer traffic (distinct from the `api_addr` used by clients). Communication uses gRPC (defined in `raft.proto`).

## Request Flow

### Produce

```
Producer.send_sync("hello")
  → accumulate in batch buffer
  → flush() → KafkaBrokerClient.Produce(ProduceRequest) [gRPC]
    → KafkaBrokerServer.produce() [Tonic handler]
      → BrokerGrpcRequest::Produce sent over mpsc channel
        → BrokerCore.handle_produce()
          → storage.produce_message(topic, partition, message)
            → [InMemory] push to HashMap, return offset
            → [Raft] propose BrokerCommand::Produce, await commit
          → ProduceResponse { offset }
      → one-shot response channel → gRPC response
```

### Fetch

```
Consumer poll loop tick
  → KafkaBrokerClient.Fetch(FetchRequest) [gRPC]
    → KafkaBrokerServer.fetch() [Tonic handler]
      → BrokerGrpcRequest::Fetch sent over mpsc channel
        → BrokerCore.handle_fetch()
          → storage.fetch_messages(topic, partition, offset, max_bytes)
            → return Vec<Message> from offset
          → FetchResponse { messages }
      → one-shot response channel → gRPC response
  → MessageHandler.handle(ConsumedMessage) for each message
  → [optional] commit_offset() on auto-commit interval
```

## Design Decisions

### Generic Storage via Trait

Using a `BrokerStorage` trait rather than an enum makes it straightforward to add new storage backends (e.g., disk-backed, tiered storage) without modifying the broker core or gRPC server. It also enables clean unit testing with mock implementations.

### mpsc Channel Between gRPC and Core

Tonic requires handlers to be `Send + 'static`. Routing requests through a channel allows the storage layer to be owned by a single async task without needing to wrap it in `Arc<Mutex<>>`. The one-shot channel per request provides an efficient request/response pairing.

### Separate api_addr and rpc_addr

Separating client-facing API traffic from Raft peer traffic allows independent network policies (e.g., firewall rules, separate network interfaces) and prevents consumer/producer traffic from competing with consensus traffic.

### Batch Accumulation on the Producer

Batching on the client side (rather than the broker side) keeps the broker simple and puts latency/throughput control in the producer's hands. Applications with strict latency requirements can use `send_sync()` to bypass batching entirely.
