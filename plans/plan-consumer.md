# Feature: Consumer

## 1. Purpose

The Consumer is the client-side component responsible for reading messages from a broker topic partition. It runs a background poll loop that periodically fetches message batches, delivers each message to a user-supplied `MessageHandler`, and optionally commits the consumed offset back to the broker on a configurable interval.

The Consumer abstracts offset tracking and group coordination so application code only needs to implement a single `handle()` method.

## 2. Responsibilities

- Resolve the starting offset on startup: consult committed group offset first, then fall back to the configured sentinel (`OFFSET_EARLIEST` or `OFFSET_LATEST`)
- Run a background poll loop that calls `Fetch` on the broker at `poll_interval_ms`
- Deliver each fetched message to the `MessageHandler`; advance the local offset pointer after each successful delivery
- Optionally commit the local offset to the broker every `auto_commit_interval_ms` when `auto_commit = true`
- Expose `commit()` for manual offset management
- Expose `seek(offset)` to reposition the consumer to any offset (overrides committed offset)
- Expose `poll()` for applications that prefer explicit control over the poll loop
- Expose `shutdown()` to stop the background task and flush a final commit if `auto_commit` is enabled

## 3. Non-Responsibilities

- Does not participate in Kafka-style partition rebalancing (one consumer per `(topic, partition)`)
- Does not deserialize message payloads (payloads are opaque `Vec<u8>`)
- Does not perform leader discovery; uses the single endpoint in `ConsumerConfig`
- Does not retry on transient network errors automatically (retry policy is on `KafkaBrokerClient`)
- Does not enforce exactly-once delivery (at-least-once is possible; exactly-once requires idempotent handlers)

## 4. Architecture Design

```
Application implements MessageHandler
    |
    | consumer.start(handler)
    v
+---------------------------------------------------+
|                     Consumer                      |
|                                                   |
|  current_offset: i64                              |
|  last_commit_offset: i64                          |
|  config: ConsumerConfig                           |
|  client: KafkaBrokerClient                        |
|                                                   |
|  Background poll task:                            |
|    tick(poll_interval_ms)                         |
|      → client.fetch(topic, partition, offset)     |
|      → for msg in response: handler.handle(msg)  |
|      → advance offset                             |
|      → if auto_commit tick: client.commit_offset()|
+---------------------------------------------------+
    |
    | FetchRequest / OffsetCommitRequest (gRPC)
    v
KafkaBrokerClient → KafkaBrokerServer → BrokerCore → BrokerStorage
```

### Offset Resolution on Startup

```
startup:
  committed = client.fetch_offset(group_id, topic, partition).await?
  if committed is Some(o):
    current_offset = o + 1          // resume after last committed
  else if config.offset == OFFSET_EARLIEST:
    offsets = client.list_offsets(topic, partition, EARLIEST).await?
    current_offset = offsets.earliest  // typically 0
  else if config.offset == OFFSET_LATEST:
    offsets = client.list_offsets(topic, partition, LATEST).await?
    current_offset = offsets.latest    // skip existing messages
  else:
    current_offset = config.offset    // exact position from config
```

## 5. Core Data Structures (Rust)

```rust
// src/client/consumer.rs

/// A message delivered to the MessageHandler.
#[derive(Debug, Clone)]
pub struct ConsumedMessage {
    pub topic:     String,
    pub partition: i32,
    pub offset:    i64,
    pub key:       Option<Vec<u8>>,
    pub value:     Vec<u8>,
}

/// Application-defined message processing logic.
#[async_trait]
pub trait MessageHandler: Send + Sync + 'static {
    /// Process one message. Return Ok(()) to acknowledge; return Err to log and skip.
    /// The consumer does not stop on errors — it logs and continues.
    async fn handle(&self, message: ConsumedMessage) -> anyhow::Result<()>;
}

/// Long-running consumer with background poll loop.
pub struct Consumer<C: KafkaBrokerClientTrait> {
    client: C,
    config: ConsumerConfig,
    /// Current read position in the partition.
    current_offset: i64,
    /// Last offset sent to the broker via CommitOffset.
    last_commit_offset: i64,
    /// Handle to the background poll task (set after start() is called).
    poll_task: Option<JoinHandle<()>>,
    /// Signal to stop the background poll task.
    shutdown_tx: Option<oneshot::Sender<()>>,
}

// src/client/config.rs

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ConsumerConfig {
    pub topic:                   String,
    pub partition:               i32,
    pub group_id:                String,
    /// Starting offset sentinel or exact offset.
    pub offset:                  i64,
    pub max_bytes:               i32,
    pub max_wait_ms:             i32,
    pub min_bytes:               i32,
    pub auto_commit:             bool,
    pub auto_commit_interval_ms: u64,
    pub poll_interval_ms:        u64,
}
```

## 6. Public Interfaces

```rust
impl<C: KafkaBrokerClientTrait> Consumer<C> {
    /// Create a Consumer. Does NOT yet start polling.
    pub fn new(client: C, config: ConsumerConfig) -> Self;

    /// Resolve starting offset and spawn the background poll task.
    /// Returns immediately; polling happens in the background.
    pub async fn start<H: MessageHandler>(&mut self, handler: H) -> anyhow::Result<()>;

    /// Fetch one batch of messages synchronously. Does not use the background task.
    /// Useful for manual poll loops.
    pub async fn poll(&mut self) -> anyhow::Result<Vec<ConsumedMessage>>;

    /// Commit `current_offset - 1` to the broker for the configured group.
    pub async fn commit(&mut self) -> anyhow::Result<()>;

    /// Reposition to `offset`. Takes effect on the next poll.
    pub fn seek(&mut self, offset: i64);

    /// Stop the background task; optionally flush a final commit.
    pub async fn shutdown(mut self) -> anyhow::Result<()>;
}
```

## 7. Internal Algorithms

### start

```
start(handler):
  self.current_offset = resolve_starting_offset().await?
  (shutdown_tx, shutdown_rx) = oneshot::channel()
  self.shutdown_tx = Some(shutdown_tx)

  // Clone shared state into the task
  client = self.client.clone()
  config = self.config.clone()
  current_offset = self.current_offset  // task owns its own copy
  last_commit = current_offset

  self.poll_task = Some(tokio::spawn(async move {
    poll_loop(client, config, handler, current_offset, last_commit, shutdown_rx).await
  }))
```

### Background poll_loop

```
poll_loop(client, config, handler, mut current_offset, mut last_commit, shutdown_rx):
  poll_interval = tokio::time::interval(config.poll_interval_ms)
  auto_commit_interval = tokio::time::interval(config.auto_commit_interval_ms)

  loop:
    select!:
      _ = poll_interval.tick() →
        msgs = fetch_batch(client, config, current_offset).await
        if Err(e): log::warn!("fetch error: {e}"); continue
        for msg in msgs:
          if let Err(e) = handler.handle(msg.clone()).await:
            log::warn!("handler error at offset {}: {e}", msg.offset)
          current_offset = msg.offset + 1

      _ = auto_commit_interval.tick() if config.auto_commit →
        if current_offset > last_commit:
          commit_offset(client, config, current_offset - 1).await
          last_commit = current_offset

      _ = shutdown_rx →
        if config.auto_commit && current_offset > last_commit:
          commit_offset(client, config, current_offset - 1).await
        break
```

### fetch_batch

```
fetch_batch(client, config, offset):
  req = FetchRequest {
    replica_id: -1,
    max_wait_ms: config.max_wait_ms,
    min_bytes: config.min_bytes,
    topics: [{
      topic: config.topic,
      partitions: [{ partition: config.partition, fetch_offset: offset, max_bytes: config.max_bytes }]
    }]
  }
  response = client.fetch(req).await?
  extract messages from first topic → first partition → records
  map each record to ConsumedMessage { topic, partition, offset, key, value }
  return Ok(messages)
```

### resolve_starting_offset

```
resolve_starting_offset():
  response = client.fetch_offset(OffsetFetchRequest { group_id, topics: [{ topic, partitions: [partition] }] }).await?
  committed = extract committed_offset for partition from response
  if committed >= 0:       // broker returns -1 for "no committed offset"
    return committed + 1   // resume after the last consumed message

  if config.offset == OFFSET_EARLIEST (-2):
    response = client.list_offsets(topic, partition, timestamp: -2).await?
    return response.earliest_offset

  if config.offset == OFFSET_LATEST (-1):
    response = client.list_offsets(topic, partition, timestamp: -1).await?
    return response.latest_offset

  return config.offset     // use exact configured offset
```

## 8. Persistence Model

The Consumer itself is stateless between restarts. Offset state is persisted by the broker via `CommitOffset`. On the next run:
1. `Consumer::start()` calls `FetchOffset` to retrieve the last committed offset
2. Resumes from `committed_offset + 1`

If `auto_commit = false` and the application never calls `commit()`, progress is lost on restart (the consumer re-reads from the configured sentinel).

## 9. Concurrency Model

| Object | Primitive | Usage |
|---|---|---|
| Background poll task | `tokio::task::JoinHandle` | Spawned by `start()`; owns its own copy of offset and client |
| `shutdown_tx` | `oneshot::Sender<()>` | Dropped on `shutdown()` to signal the background task |
| `Consumer.current_offset` | Owned by caller task | Not shared after `start()` — the task has its own copy |

**No shared mutable state between poll task and caller**: After `start()`, the background task owns `current_offset` and `last_commit_offset`. The `Consumer` struct on the caller side is dormant until `shutdown()`. For manual control, callers use `poll()` and `commit()` directly (without calling `start()`).

## 10. Configuration

```rust
pub struct ConsumerConfig {
    pub topic:                   String,   // required
    pub partition:               i32,      // default: 0
    pub group_id:                String,   // required
    pub offset:                  i64,      // default: OFFSET_EARLIEST (-2)
    pub max_bytes:               i32,      // default: 1_048_576 (1 MiB)
    pub max_wait_ms:             i32,      // default: 1000
    pub min_bytes:               i32,      // default: 1
    pub auto_commit:             bool,     // default: true
    pub auto_commit_interval_ms: u64,      // default: 5000
    pub poll_interval_ms:        u64,      // default: 1000
}
```

## 11. Observability

- `start()`: `INFO` log with topic, partition, group_id, resolved starting offset
- Each `poll()`: `TRACE` log with topic, partition, offset, message count returned
- Each message delivered to handler: `TRACE` log with offset
- Handler error: `WARN` log with offset and error (consumer continues)
- Each `commit()`: `DEBUG` log with group_id, topic, partition, committed offset
- `shutdown()`: `INFO` log with final committed offset

## 12. Testing Strategy

**Unit tests** (using mock `KafkaBrokerClientTrait` and mock `MessageHandler`):
- `test_offset_resolution_from_committed`: mock returns committed offset 42; assert `start()` begins at 43
- `test_offset_resolution_earliest`: no committed offset; config = OFFSET_EARLIEST; assert ListOffsets called and offset 0 used
- `test_offset_resolution_latest`: no committed offset; config = OFFSET_LATEST; assert ListOffsets called and latest offset used
- `test_offset_resolution_exact`: no committed offset; config = 17; assert starts at 17
- `test_poll_delivers_messages`: mock returns 3 messages; assert handler called 3 times in order
- `test_poll_advances_offset`: poll returns msgs at offsets 0, 1, 2; assert next poll requests offset 3
- `test_auto_commit_fires_on_interval`: advance mock time by `auto_commit_interval_ms`; assert CommitOffset called
- `test_auto_commit_skipped_if_no_progress`: no messages received; assert CommitOffset not called
- `test_handler_error_does_not_stop_consumer`: handler returns Err on msg 1; assert msg 2 still delivered
- `test_seek_repositions_offset`: call `seek(99)`, then `poll()`, assert FetchRequest uses offset 99
- `test_shutdown_commits_final_offset`: buffer consumed messages, call `shutdown()`, assert CommitOffset called before return
- `test_manual_poll_loop`: call `poll()` 3 times without `start()`; assert messages accumulate correctly

## 13. Open Questions

None.
