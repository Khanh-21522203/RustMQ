# Feature: Producer

## 1. Purpose

The Producer is the client-side component responsible for publishing messages to a broker. It accumulates messages in a local batch buffer to amortize gRPC round-trip overhead, then flushes the batch to the broker either when the batch reaches a configured size limit or after a configurable time interval. It exposes two send modes — buffered (high throughput) and synchronous (low latency) — giving callers explicit control over the latency/throughput trade-off.

## 2. Responsibilities

- Accept messages from the application via `send()` and buffer them in an in-memory batch
- Spawn a background Tokio task that flushes the batch on a configurable interval (`flush_interval_ms`)
- Force-flush immediately when the batch reaches `batch_size` messages
- Expose `send_sync()` for applications that need per-message acknowledgment and the assigned offset
- Expose `flush()` to drain the batch manually and wait for all in-flight acknowledgments
- Expose `shutdown()` to flush remaining messages and stop the background task
- Thread-safely share the batch buffer between the caller tasks and the background flush task using `Arc<Mutex<Vec<ProducerMessage>>>`
- Map `ProduceResponse` error codes into typed `anyhow::Error` values for the caller

## 3. Non-Responsibilities

- Does not perform key-based partition routing (caller specifies the partition explicitly, or the broker's default partition is used)
- Does not retry on transient errors automatically (retry policy is on the `KafkaBrokerClient`)
- Does not serialize message values (payloads are opaque `Vec<u8>`)
- Does not perform leader discovery; uses the single endpoint in `ProducerConfig`
- Does not compress messages

## 4. Architecture Design

```
Application task(s)
    |
    | producer.send(msg)   producer.send_sync(msg)
    v                              v
+---+------------------------------+------------------+
|                  Producer                           |
|                                                     |
|  batch: Arc<Mutex<Vec<ProducerMessage>>>            |
|  client: KafkaBrokerClient (Arc-shared)             |
|  config: ProducerConfig                             |
|                                                     |
|  Background flush task:                             |
|    tick(flush_interval_ms) → flush_batch()          |
|    or: batch.len() >= batch_size → flush_batch()    |
+-----------------------------------------------------+
    |
    | ProduceRequest (gRPC)
    v
KafkaBrokerClient → KafkaBrokerServer → BrokerCore → BrokerStorage
```

### Flush Lifecycle

```
State: batch = [msg0, msg1, ..., msgN]

flush_batch():
  lock batch → drain messages into local Vec → unlock
  if local Vec is empty → return

  build ProduceRequest:
    topic_data = { name: config.topic, partitions: [{ index: config.partition, records: local Vec }] }
  client.produce(ProduceRequest).await → ProduceResponse
  map error codes → anyhow::Error if any partition has error_code != 0
```

## 5. Core Data Structures (Rust)

```rust
// src/client/producer.rs

/// A message to be published by the Producer.
pub struct ProducerMessage {
    /// Optional routing key. Determines partition if None partition override is given.
    pub key: Option<Vec<u8>>,
    /// Opaque payload bytes.
    pub value: Vec<u8>,
    /// Override partition. None → use ProducerConfig.partition.
    pub partition: Option<i32>,
}

/// High-throughput, batching message publisher.
pub struct Producer<C: KafkaBrokerClientTrait> {
    client: C,
    config: ProducerConfig,
    /// Shared batch buffer between caller and background flush task.
    batch: Arc<Mutex<Vec<ProducerMessage>>>,
    /// Signal to stop the background flush task.
    shutdown_tx: Option<oneshot::Sender<()>>,
    /// Join handle for the background flush task.
    flush_task: Option<JoinHandle<()>>,
}

// src/client/config.rs

/// Configuration for the Producer.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ProducerConfig {
    /// Topic to publish messages to.
    pub topic: String,
    /// Default partition for messages with no partition override.
    pub partition: i32,
    /// How many broker replicas must acknowledge a write.
    /// -1 = all, 0 = none, 1 = leader only.
    pub required_acks: i32,
    /// Produce request timeout at the broker (milliseconds).
    pub timeout_ms: i32,
    /// Maximum messages to accumulate before forcing a flush.
    pub batch_size: usize,
    /// Maximum time between flushes (milliseconds).
    pub flush_interval_ms: u64,
}
```

## 6. Public Interfaces

```rust
impl<C: KafkaBrokerClientTrait> Producer<C> {
    /// Create a Producer. Spawns the background flush task immediately.
    pub fn new(client: C, config: ProducerConfig) -> Self;

    /// Buffer a message. Returns after the message is added to the local batch.
    /// If the batch reaches `batch_size`, triggers an immediate flush.
    pub async fn send(&self, msg: ProducerMessage) -> anyhow::Result<()>;

    /// Flush immediately and wait for the broker to acknowledge the message.
    /// Returns the offset assigned by the broker.
    pub async fn send_sync(&self, msg: ProducerMessage) -> anyhow::Result<i64>;

    /// Flush all buffered messages and wait for broker acknowledgment.
    pub async fn flush(&self) -> anyhow::Result<()>;

    /// Flush pending messages, stop the background task, and release resources.
    pub async fn shutdown(mut self) -> anyhow::Result<()>;
}
```

## 7. Internal Algorithms

### send (buffered)

```
send(msg):
  batch = self.batch.lock().await
  batch.push(msg)
  if batch.len() >= self.config.batch_size:
    let to_send = drain(batch)  // take all, release lock
    drop(batch lock)
    flush_messages(to_send).await?
  return Ok(())
```

### send_sync (synchronous)

```
send_sync(msg):
  partition = msg.partition.unwrap_or(self.config.partition)
  req = ProduceRequest {
    required_acks: self.config.required_acks,
    timeout_ms:    self.config.timeout_ms,
    topics: [{
      name: self.config.topic,
      partitions: [{ index: partition, records: [msg] }]
    }]
  }
  response = self.client.produce(req).await?
  offset = extract_base_offset(response, topic, partition)?
  return Ok(offset)
```

### Background flush task

```
spawn:
  interval = tokio::time::interval(flush_interval_ms)
  loop:
    select!:
      _ = interval.tick() →
        batch = self.batch.lock().await
        if batch.is_empty(): continue
        to_send = drain(batch)
        drop(batch lock)
        if let Err(e) = flush_messages(to_send).await:
          log::warn!("background flush error: {e}")
      _ = shutdown_rx →
        break
```

### flush_messages (shared by flush paths)

```
flush_messages(messages: Vec<ProducerMessage>):
  // Group by partition (in case messages have different partition overrides)
  by_partition: HashMap<i32, Vec<ProducerMessage>> = group(messages)
  partition_data = by_partition.into_iter().map(|(p, msgs)| {
    PartitionProduceData { index: p, records: msgs.into_proto() }
  }).collect()
  req = ProduceRequest {
    required_acks: self.config.required_acks,
    timeout_ms:    self.config.timeout_ms,
    topics: [{ name: self.config.topic, partitions: partition_data }]
  }
  response = self.client.produce(req).await?
  check all partition error codes; return first error if any
  return Ok(())
```

### shutdown

```
shutdown():
  // Signal background task to stop
  drop(self.shutdown_tx)
  // Flush any remaining messages
  self.flush().await?
  // Wait for background task to exit
  if let Some(handle) = self.flush_task:
    handle.await.ok()
  return Ok(())
```

## 8. Persistence Model

The Producer is entirely in-memory. The batch buffer is a `Vec` in process memory. If the process crashes before a flush, buffered messages are lost. Applications requiring at-least-once delivery should use `send_sync()` or call `flush()` before exiting.

## 9. Concurrency Model

| Object | Primitive | Usage |
|---|---|---|
| `Producer.batch` | `Arc<tokio::sync::Mutex<Vec<ProducerMessage>>>` | Caller tasks lock to append; background task locks to drain |
| Background flush task | `tokio::task::JoinHandle` | Spawned at construction; stopped via `shutdown_tx` |
| `shutdown_tx` | `oneshot::Sender<()>` | Dropped on shutdown to signal the background task |

**Lock duration**: The batch lock is held only during the `Vec::push` or `Vec::drain` operation — never across an `.await`. This prevents the background flush task from blocking callers during a network call.

## 10. Configuration

```rust
pub struct ProducerConfig {
    pub topic:             String,   // required
    pub partition:         i32,      // default: 0
    pub required_acks:     i32,      // default: 1
    pub timeout_ms:        i32,      // default: 5000
    pub batch_size:        usize,    // default: 100
    pub flush_interval_ms: u64,      // default: 100
}
```

## 11. Observability

- `send()` called: `TRACE` log with topic, partition, current batch size
- Batch flushed: `DEBUG` log with message count, topic, partition
- Flush error (background task): `WARN` log with error, will retry on next tick
- `shutdown()`: `INFO` log with total messages sent during lifetime

## 12. Testing Strategy

**Unit tests** (using mock `KafkaBrokerClientTrait`):
- `test_send_buffers_message`: call `send()`, assert batch has 1 message, no gRPC call made
- `test_batch_size_triggers_flush`: call `send()` `batch_size` times, assert gRPC produce called exactly once
- `test_send_sync_produces_immediately`: call `send_sync()`, assert gRPC produce called before function returns, returned offset matches mock
- `test_flush_drains_batch`: buffer 3 messages, call `flush()`, assert batch empty and gRPC called with 3 records
- `test_flush_noop_on_empty_batch`: call `flush()` with empty batch, assert no gRPC call
- `test_background_flush_fires_on_interval`: buffer message, advance mock time by `flush_interval_ms`, assert gRPC called
- `test_shutdown_flushes_remaining`: buffer 2 messages, call `shutdown()`, assert gRPC called with 2 records before return
- `test_concurrent_senders`: 10 tasks each call `send()` 100 times concurrently; assert total records in gRPC calls == 1000

## 13. Open Questions

None.
