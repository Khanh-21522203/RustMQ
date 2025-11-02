# Broker Core Implementation

## Overview

This document describes the broker core implementation following SOLID principles with a clean architecture.

## Architecture

### Components

1. **Storage Layer** (`src/broker/storage.rs`)
   - **BrokerStorage Trait**: Interface defining all storage operations
   - **InMemoryStorage**: Concrete implementation storing data in memory using HashMaps
   - Easily extensible to add disk-based or distributed storage implementations

2. **Core Layer** (`src/broker/core.rs`)
   - **BrokerCore**: Main broker logic that processes RPC requests
   - Generic over storage type `<S: BrokerStorage>`
   - Handles all Kafka protocol operations

3. **Server Layer** (`src/broker/kafka_broker_server.rs`)
   - **KafkaBrokerServer**: gRPC server implementation
   - Bridges gRPC requests to BrokerCore via channels

## Design Principles

### Single Responsibility Principle (SRP)
- **Storage**: Only handles data persistence
- **Core**: Only handles business logic
- **Server**: Only handles gRPC protocol

### Open/Closed Principle (OCP)
- Storage behavior is defined by trait, allowing new implementations without modifying existing code
- New storage backends (e.g., RocksDB, PostgreSQL) can be added by implementing `BrokerStorage`

### Liskov Substitution Principle (LSP)
- Any `BrokerStorage` implementation can be used interchangeably
- Generic type parameter ensures compile-time safety

### Interface Segregation Principle (ISP)
- `BrokerStorage` trait defines cohesive operations related to message broker storage
- Each method has a single, well-defined purpose

### Dependency Inversion Principle (DIP)
- `BrokerCore` depends on `BrokerStorage` abstraction, not concrete implementations
- High-level modules (Core) don't depend on low-level modules (InMemoryStorage)

## Data Model

### InMemoryStorage Structure

```rust
messages: HashMap<String, HashMap<i32, Vec<(i64, Vec<u8>)>>>
// topic -> partition -> [(offset, message_data)]

offsets: HashMap<String, HashMap<String, HashMap<i32, (i64, String)>>>
// consumer_group -> topic -> partition -> (offset, metadata)

groups: HashMap<String, GroupState>
// group_id -> GroupState (generation_id, leader_id, members)
```

## Implemented Features

### Topic Management
- **get_topic_metadata**: Returns metadata about topics and partitions

### Producer Operations
- **produce**: Writes messages to topics/partitions
- Messages are appended with auto-incrementing offsets

### Consumer Operations
- **fetch**: Retrieves messages from specified offset
- **list_offsets**: Gets offset information (earliest, latest)

### Consumer Group Coordination
- **find_coordinator**: Returns coordinator info for a group
- **join_group**: Adds member to consumer group
- **sync_group**: Synchronizes partition assignments
- **heartbeat**: Keeps consumer group membership alive
- **leave_group**: Removes member from group

### Offset Management
- **commit_offset**: Commits consumer group offsets
- **fetch_offset**: Retrieves committed offsets

## Error Handling

All operations return appropriate error codes:
- **0**: Success
- **1**: Generic error (with logging)

Errors are logged using the `log` crate for debugging.

## Concurrency

- Storage is wrapped in `Arc<Mutex<S>>` for thread-safe access
- Async operations using Tokio runtime
- Channel-based communication between server and core

## Usage Example

```rust
use tokio::sync::mpsc;
use crate::broker::storage::InMemoryStorage;
use crate::broker::core::BrokerCore;

// Create storage
let storage = InMemoryStorage::new(1, "localhost".to_string(), 9092);

// Create channel for RPC communication
let (tx, rx) = mpsc::channel(100);

// Create broker core
let mut broker = BrokerCore::new(rx, storage);

// Run broker (in separate task)
tokio::spawn(async move {
    broker.run().await;
});
```

## Future Improvements

1. **Persistence**: Implement disk-based storage
2. **Replication**: Add support for replica management
3. **Partitioning**: Implement partition distribution strategies
4. **Compaction**: Add log compaction for topics
5. **Authentication**: Add security layer
6. **Metrics**: Add monitoring and metrics collection
7. **Configuration**: Add configurable parameters (retention, segment size, etc.)

## Testing

To add tests, implement a mock `BrokerStorage` for unit testing:

```rust
struct MockStorage {
    // Test data
}

#[async_trait]
impl BrokerStorage for MockStorage {
    // Mock implementations
}
```

## Dependencies

- `tokio`: Async runtime
- `async-trait`: Async trait support
- `log`: Logging facade
- `tonic`: gRPC framework
