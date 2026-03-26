# Single-Node Deployment

A single-node deployment runs one broker with in-memory storage. It is the simplest way to use Rust-MQ and is suitable for:

- Local development and experimentation
- Integration testing
- Environments where data persistence is not required

**Data is not persisted** — the broker's state is lost when the process exits.

## Start the Broker

### With Default Settings

```bash
cargo run --release -- --mode broker
```

The broker listens on `localhost:50051` by default.

### With a Config File

```yaml
# config/broker-single.yaml
node_id: 1
api_addr: "0.0.0.0:50051"
rpc_addr: "0.0.0.0:50052"
log_level: "info"
```

```bash
cargo run --release -- --mode broker --config config/broker-single.yaml
```

### From the Library

```rust
use rust_mq::broker::{
    core::BrokerCore,
    kafka_broker_server::KafkaBrokerServer,
    storage::InMemoryStorage,
};
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (rpc_tx, rpc_rx) = mpsc::channel(1000);
    let storage = InMemoryStorage::new(1, "localhost".to_string(), 50051);
    let broker_core = BrokerCore::new(rpc_rx, storage);

    tokio::spawn(async move {
        broker_core.run().await;
    });

    let grpc_server = KafkaBrokerServer::new(rpc_tx);
    grpc_server.run("127.0.0.1:50051").await?;
    Ok(())
}
```

## Verify It Is Running

```bash
# Send a test message
echo "hello" | cargo run -- --mode producer --config config/producer.yaml

# Read it back
cargo run -- --mode consumer --config config/consumer.yaml
```

## Connecting Producers and Consumers

Point your client config at the broker address:

```yaml
broker:
  address: "http://localhost:50051"
  timeout_secs: 30
  max_retries: 3
```

See [Configuration](../configuration.md) for all client options.

## Limitations

| Feature | Single-Node |
|---|---|
| High availability | No — single point of failure |
| Data persistence | No — in-memory only |
| Throughput | High — no consensus overhead |
| Suitable for production | No |

For production workloads, use a [3-node Raft cluster](./cluster.md).
