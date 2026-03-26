# Client Library Reference

The Rust client library provides `Producer` and `Consumer` types that handle the details of batching, offset management, and broker communication.

## Add to Your Project

```toml
# Cargo.toml
[dependencies]
rust-mq = { path = "../Rust-MQ" }
tokio = { version = "1", features = ["full"] }
```

---

## Producer

### Creating a Producer

```rust
use rust_mq::client::{Producer, ProducerConfig};

let config = ProducerConfig {
    topic: "events".to_string(),
    partition: 0,
    partitioning: "fixed".to_string(),
    num_partitions: 1,
    required_acks: 1,
    timeout_ms: 5000,
    batch_size: 100,
    flush_interval_ms: 100,
};
let producer = Producer::new("http://localhost:50051", config).await?;
```

`config` is a `ProducerConfig` struct (see [Configuration](../configuration.md)).

### Sending Messages

#### Buffered Send (High Throughput)

```rust
use rust_mq::client::ProducerMessage;

// Enqueue a message — returns after the message is buffered locally.
// The message will be sent to the broker when the batch flushes.
producer.send(ProducerMessage {
    key: Some(b"order-123".to_vec()),
    value: b"payload bytes".to_vec(),
    partition: None,  // use default from config
}).await?;
```

#### Synchronous Send (Low Latency)

```rust
// Flush immediately and wait for broker acknowledgment.
// Returns partition + offset metadata.
let result = producer.send_sync(ProducerMessage {
    key: None,
    value: b"urgent message".to_vec(),
    partition: None,
}).await?;

println!("Message stored at partition {} offset {}", result.partition, result.offset);
```

#### Manual Flush

```rust
// Flush all buffered messages and wait for acknowledgments.
producer.flush().await?;
```

### Shutdown

```rust
// Flush pending messages and stop the background flush task.
producer.shutdown().await?;
```

### ProducerMessage Fields

| Field | Type | Description |
|---|---|---|
| `key` | `Option<Vec<u8>>` | Optional routing key; same key always goes to the same partition |
| `value` | `Vec<u8>` | Message payload (opaque bytes) |
| `partition` | `Option<i32>` | Override the partition; `None` uses the config default |

---

## Consumer

### Creating a Consumer

```rust
use rust_mq::client::{Consumer, ConsumerConfig};

let config = ConsumerConfig {
    topic: "events".to_string(),
    partitions: vec![0],
    group_id: Some("my-group".to_string()),
    offset: -2,
    max_bytes: 1_048_576,
    max_wait_ms: 1000,
    min_bytes: 1,
    auto_commit: true,
    auto_commit_interval_ms: 5000,
    poll_interval_ms: 1000,
};
let mut consumer = Consumer::new("http://localhost:50051", config).await?;
```

### Implementing a Message Handler

Implement the `MessageHandler` trait to define how your application processes messages:

```rust
use rust_mq::client::{MessageHandler, ConsumedMessage};
use async_trait::async_trait;

struct MyHandler;

#[async_trait]
impl MessageHandler for MyHandler {
    async fn handle(&self, message: ConsumedMessage) -> anyhow::Result<()> {
        println!(
            "topic={} partition={} offset={} value={:?}",
            message.topic, message.partition, message.offset, message.value
        );
        Ok(())
    }
}
```

If `handle()` returns an error, the consumer logs it and continues processing subsequent messages.

### Starting the Consumer

```rust
// Starts a background polling task. Returns immediately.
consumer.start(MyHandler).await?;

// Keep the main task alive while the consumer runs.
tokio::signal::ctrl_c().await?;
consumer.shutdown().await?;
```

### Manual Poll

Instead of the background task, you can poll explicitly:

```rust
loop {
    let messages = consumer.poll().await?;
    for msg in messages {
        process(msg).await?;
    }
    consumer.commit().await?;
}
```

### Seeking

To move to a specific offset (bypasses committed offsets):

```rust
consumer.seek(partition, offset);
```

### Shutdown

```rust
consumer.shutdown().await?;
```

### ConsumedMessage Fields

| Field | Type | Description |
|---|---|---|
| `topic` | `String` | Topic the message came from |
| `partition` | `i32` | Partition the message came from |
| `offset` | `i64` | Offset of this message within the partition |
| `key` | `Option<Vec<u8>>` | Message key (may be empty) |
| `value` | `Vec<u8>` | Message payload |

---

## Error Handling

Both `Producer` and `Consumer` return `anyhow::Result<T>` from async methods. Errors include:

- **Transport errors**: gRPC connection failures, timeouts
- **Broker errors**: Unknown topic/partition, offset out of range, not leader
- **Configuration errors**: Invalid settings

---

## Complete Example

```rust
use rust_mq::client::{
    Consumer, ConsumerConfig, MessageHandler, ConsumedMessage, Producer, ProducerConfig, ProducerMessage,
};
use async_trait::async_trait;

struct PrintHandler;

#[async_trait]
impl MessageHandler for PrintHandler {
    async fn handle(&self, msg: ConsumedMessage) -> anyhow::Result<()> {
        println!("[offset {}] {}", msg.offset, String::from_utf8_lossy(&msg.value));
        Ok(())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let producer_config = ProducerConfig {
        topic: "events".to_string(),
        partition: 0,
        partitioning: "fixed".to_string(),
        num_partitions: 1,
        required_acks: 1,
        timeout_ms: 5000,
        batch_size: 100,
        flush_interval_ms: 100,
    };

    // Producer
    let producer = Producer::new("http://localhost:50051", producer_config).await?;
    for i in 0..10 {
        producer.send(ProducerMessage {
            key: None,
            value: format!("message #{i}").into_bytes(),
            partition: None,
        }).await?;
    }
    producer.flush().await?;
    producer.shutdown().await?;

    // Consumer
    let consumer_config = ConsumerConfig {
        topic: "events".to_string(),
        partitions: vec![0],
        group_id: Some("example-group".to_string()),
        offset: -2,
        max_bytes: 1_048_576,
        max_wait_ms: 1000,
        min_bytes: 1,
        auto_commit: true,
        auto_commit_interval_ms: 5000,
        poll_interval_ms: 1000,
    };

    let mut consumer = Consumer::new("http://localhost:50051", consumer_config).await?;
    consumer.start(PrintHandler).await?;

    tokio::signal::ctrl_c().await?;
    consumer.shutdown().await?;

    Ok(())
}
```

See `examples/producer_consumer.rs` in the repository for a complete runnable example.
