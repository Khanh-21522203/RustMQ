# Feature: Broker Core

## 1. Purpose

Broker Core (`BrokerCore<S>`) is the request dispatcher that sits between the gRPC transport layer and the storage backend. It owns the mpsc receiver, deserializes typed request variants from `BrokerGrpcRequest`, dispatches them to the appropriate `BrokerStorage` method, and sends typed responses back to the gRPC handler via one-shot channels.

By separating business logic from transport, `BrokerCore` can be unit-tested against a mock storage implementation without any gRPC setup. It is the single place that contains offset resolution, error mapping, and response construction.

## 2. Responsibilities

- Own the `mpsc::Receiver<BrokerGrpcRequest>` and drive the request loop
- Dispatch each request variant to the appropriate `BrokerStorage` method
- Perform offset resolution: translate `OFFSET_EARLIEST` / `OFFSET_LATEST` sentinels into concrete offsets by calling `storage.earliest_offset()` / `storage.latest_offset()`
- Map `anyhow::Error` results to Kafka-compatible `ErrorCode` values in the response
- Construct and send the typed `BrokerGrpcResponse` back through the one-shot channel
- Run as a single `tokio::task::spawn` — no internal parallelism; all storage calls are `await`ed sequentially within the loop
- Log every request at `DEBUG` and every error at `WARN`

## 3. Non-Responsibilities

- Does not deserialize protobuf bytes (that is the gRPC server's job)
- Does not manage TCP connections or TLS
- Does not apply backpressure or rate limiting
- Does not perform Raft coordination (delegated to `MultiBroker` via the `BrokerStorage` trait)
- Does not persist state (all persistence is behind the `BrokerStorage` trait)

## 4. Architecture Design

```
KafkaBrokerServer (Tonic gRPC handlers, one per RPC)
      |
      | sends BrokerGrpcRequest + oneshot::Sender<BrokerGrpcResponse>
      v
 mpsc::channel (unbounded)
      |
      v
+----------------------------------------------+
|             BrokerCore<S>                    |
|                                              |
|  loop:                                       |
|    recv BrokerGrpcRequest                    |
|    dispatch to handler method                |
|    await BrokerStorage method                |
|    send BrokerGrpcResponse via oneshot       |
+----------------------------------------------+
      |
      | async trait calls
      v
BrokerStorage (InMemoryStorage | MultiBroker)
```

**Channel pattern**: Each request carries a `oneshot::Sender<BrokerGrpcResponse>`. The gRPC handler creates the `(tx, rx)` pair, sends the request with `tx`, and then `.await`s `rx`. `BrokerCore` calls `tx.send(response)` after handling.

```
gRPC handler                     BrokerCore
    |                                |
    |  let (tx, rx) = oneshot()      |
    |  mpsc_tx.send(req, tx)         |
    |-------------------------------->|
    |  rx.await                      |  await storage.xxx()
    |                                |  tx.send(response)
    |<--------------------------------|
    |  return gRPC response           |
```

## 5. Core Data Structures (Rust)

```rust
// src/broker/core.rs

use tokio::sync::{mpsc, oneshot};

/// BrokerCore drives the request loop for a single broker node.
pub struct BrokerCore<S: BrokerStorage> {
    storage: S,
    receiver: mpsc::UnboundedReceiver<(BrokerGrpcRequest, oneshot::Sender<BrokerGrpcResponse>)>,
}

// src/api/requests.rs

/// Wraps all possible gRPC request types in a single enum.
/// Each variant carries the deserialized protobuf request struct.
pub enum BrokerGrpcRequest {
    GetTopicMetadata(MetadataRequest),
    Produce(ProduceRequest),
    Fetch(FetchRequest),
    ListOffsets(ListOffsetsRequest),
    FindCoordinator(FindCoordinatorRequest),
    JoinGroup(JoinGroupRequest),
    SyncGroup(SyncGroupRequest),
    Heartbeat(HeartbeatRequest),
    LeaveGroup(LeaveGroupRequest),
    CommitOffset(OffsetCommitRequest),
    FetchOffset(OffsetFetchRequest),
}

// src/api/responses.rs

/// Wraps all possible gRPC response types in a single enum.
pub enum BrokerGrpcResponse {
    GetTopicMetadata(MetadataResponse),
    Produce(ProduceResponse),
    Fetch(FetchResponse),
    ListOffsets(ListOffsetsResponse),
    FindCoordinator(FindCoordinatorResponse),
    JoinGroup(JoinGroupResponse),
    SyncGroup(SyncGroupResponse),
    Heartbeat(HeartbeatResponse),
    LeaveGroup(LeaveGroupResponse),
    CommitOffset(OffsetCommitResponse),
    FetchOffset(OffsetFetchResponse),
}
```

## 6. Public Interfaces

```rust
impl<S: BrokerStorage> BrokerCore<S> {
    /// Create a BrokerCore with the given storage backend and mpsc receiver.
    pub fn new(
        storage: S,
        receiver: mpsc::UnboundedReceiver<(BrokerGrpcRequest, oneshot::Sender<BrokerGrpcResponse>)>,
    ) -> Self;

    /// Start the request loop. This future runs until the mpsc channel is closed.
    /// Spawn this on a dedicated Tokio task:
    ///   tokio::spawn(core.run());
    pub async fn run(mut self);
}
```

## 7. Internal Algorithms

### Main Request Loop

```
BrokerCore::run():
  loop:
    match self.receiver.recv().await:
      None → break  // all senders dropped; broker shutting down
      Some((request, reply_tx)) →
        response = self.dispatch(request).await
        let _ = reply_tx.send(response)  // ignore if receiver dropped
```

### Dispatch

```
dispatch(request) -> BrokerGrpcResponse:
  match request:
    Produce(req)          → handle_produce(req).await
    Fetch(req)            → handle_fetch(req).await
    ListOffsets(req)      → handle_list_offsets(req).await
    GetTopicMetadata(req) → handle_get_topic_metadata(req).await
    CommitOffset(req)     → handle_commit_offset(req).await
    FetchOffset(req)      → handle_fetch_offset(req).await
    JoinGroup(req)        → handle_join_group(req).await
    SyncGroup(req)        → handle_sync_group(req).await
    Heartbeat(req)        → handle_heartbeat(req).await
    LeaveGroup(req)       → handle_leave_group(req).await
    FindCoordinator(req)  → handle_find_coordinator(req).await
```

### handle_produce

```
handle_produce(req: ProduceRequest) -> BrokerGrpcResponse:
  topic_responses = []
  for topic_data in req.topics:
    partition_responses = []
    for partition_data in topic_data.partitions:
      first_offset = -1
      error_code = 0
      for msg in partition_data.records:
        match storage.produce_message(topic, partition, msg.key, msg.value).await:
          Ok(offset) → if first_offset == -1: first_offset = offset
          Err(e)     → error_code = map_error(e); break
      partition_responses.push(PartitionProduceResponse {
        index: partition_data.index,
        error_code,
        base_offset: first_offset,
      })
    topic_responses.push(TopicProduceResponse { name: topic_data.name, partitions: partition_responses })
  BrokerGrpcResponse::Produce(ProduceResponse { topics: topic_responses })
```

### handle_fetch (with sentinel resolution)

```
handle_fetch(req: FetchRequest) -> BrokerGrpcResponse:
  topic_responses = []
  for fetch_topic in req.topics:
    partition_responses = []
    for fetch_partition in fetch_topic.partitions:
      start_offset = fetch_partition.fetch_offset
      // Resolve sentinels
      if start_offset == OFFSET_EARLIEST:
        start_offset = storage.earliest_offset(topic, partition).await.unwrap_or(0)
      else if start_offset == OFFSET_LATEST:
        start_offset = storage.latest_offset(topic, partition).await.unwrap_or(0)
      match storage.fetch_messages(topic, partition, start_offset, fetch_partition.max_bytes).await:
        Ok(msgs) →
          high_watermark = storage.latest_offset(topic, partition).await.unwrap_or(0)
          partition_responses.push(FetchPartitionResponse { msgs, high_watermark, error_code: 0 })
        Err(e) →
          partition_responses.push(FetchPartitionResponse { error_code: map_error(e), ... })
    topic_responses.push(...)
  BrokerGrpcResponse::Fetch(FetchResponse { topics: topic_responses })
```

### Error Mapping

```
map_error(e: anyhow::Error) -> i32:
  // Inspect error string or downcast to known error types
  if e contains "not leader" → ErrorCode::NotLeaderForPartition as i32
  if e contains "unknown topic" → ErrorCode::UnknownTopicOrPartition as i32
  if e contains "offset out of range" → ErrorCode::OffsetOutOfRange as i32
  if e contains "unknown member" → ErrorCode::UnknownMemberId as i32
  if e contains "illegal generation" → ErrorCode::IllegalGeneration as i32
  else → -1  // generic unknown error
```

## 8. Persistence Model

`BrokerCore` holds no state of its own. All state is behind the `BrokerStorage` trait. Persistence characteristics depend entirely on the chosen implementation:
- `InMemoryStorage`: no persistence
- `MultiBroker`: Raft log on disk in `storage_path`

## 9. Concurrency Model

| Object | Primitive | Usage |
|---|---|---|
| `BrokerCore` task | `tokio::spawn` | Single dedicated task; processes requests one at a time |
| mpsc channel | `tokio::sync::mpsc::UnboundedReceiver` | Multiple gRPC handler tasks send; one BrokerCore task receives |
| Response channels | `oneshot::Sender<BrokerGrpcResponse>` | Per-request; gRPC handler awaits; BrokerCore sends |

**Single-task design**: `BrokerCore` processes one request at a time. This is intentional — storage ordering guarantees are simpler when there is one writer. Throughput is bound by storage latency, not by parallelism limits at this layer.

If higher throughput is needed in the future, `BrokerCore` can be sharded by topic-partition: one task per partition, each with its own channel.

## 10. Configuration

`BrokerCore` has no configuration of its own. The mpsc channel buffer size is determined by the caller (currently unbounded — backpressure is handled by the storage layer).

## 11. Observability

- Every request type logged at `DEBUG`: `"handling {request_type} for topic={} partition={}"`
- Every storage error logged at `WARN`: `"storage error on {request_type}: {error}"`
- Request count per type: `TRACE`-level counter (future: Prometheus counter per variant)
- Channel closure (broker shutdown): logged at `INFO`

## 12. Testing Strategy

**Unit tests** (using `InMemoryStorage` as the backend):
- `test_produce_then_fetch`: send Produce request, send Fetch request, assert messages match
- `test_fetch_offset_sentinel_earliest`: send Fetch with `OFFSET_EARLIEST`, assert resolves to offset 0
- `test_fetch_offset_sentinel_latest`: send Fetch with `OFFSET_LATEST` on non-empty partition, assert resolves to `message_count`
- `test_commit_and_fetch_offset`: send CommitOffset, send FetchOffset, assert returned value matches
- `test_list_offsets_empty_partition`: ListOffsets on unknown partition, assert earliest=0 and latest=0
- `test_error_propagation`: configure storage to return an error; assert error code in response is non-zero
- `test_core_shuts_down_when_channel_closed`: drop all mpsc senders, assert `run()` future completes

**Integration tests** (full gRPC stack):
- See `plan-grpc-transport.md` §12

## 13. Open Questions

None.
