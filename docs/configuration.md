# Configuration Reference

All configuration is loaded from YAML files. The same file format is used for both client (producer/consumer) and broker configurations.

## Broker Configuration

Used when running `--mode broker`.

```yaml
# Unique identifier for this node within the cluster.
# Must be a positive integer; must match the node_id in initial_members.
node_id: 1

# Address that client applications connect to (gRPC API).
api_addr: "127.0.0.1:9092"

# Address used for Raft peer-to-peer communication (internal, not client-facing).
rpc_addr: "127.0.0.1:19092"

# Directory for persistent state (Raft log, snapshots).
# Only used in cluster mode; ignored for single-node in-memory deployments.
storage_path: "./data/broker-1"

# Cluster membership configuration.
# Omit the `cluster` section entirely for a single-node (in-memory) deployment.
cluster:
  # All nodes that form the initial cluster.
  # Every node in the cluster must list every other node here.
  initial_members:
    - node_id: 1
      api_addr: "127.0.0.1:9092"
      rpc_addr: "127.0.0.1:19092"
    - node_id: 2
      api_addr: "127.0.0.1:9093"
      rpc_addr: "127.0.0.1:19093"
    - node_id: 3
      api_addr: "127.0.0.1:9094"
      rpc_addr: "127.0.0.1:19094"

  # Set to true only on the very first node to bootstrap a new cluster.
  # After the cluster is established, this must be false (or omitted) on all nodes.
  bootstrap: true

# Raft tuning parameters.
raft:
  # How often the leader sends heartbeats to followers (milliseconds).
  heartbeat_interval_ms: 1000

  # Minimum and maximum election timeout (milliseconds).
  # A follower starts an election if it doesn't hear from the leader within this window.
  # Should be significantly larger than heartbeat_interval_ms.
  election_timeout_min_ms: 3000
  election_timeout_max_ms: 6000

  # After this many Raft log entries, a snapshot is taken to compact the log.
  snapshot_threshold: 10000

# Log verbosity: "error", "warn", "info", "debug", "trace"
log_level: "info"
```

### Single-Node (No Cluster Section)

For development and testing, omit `cluster` and `raft`:

```yaml
node_id: 1
api_addr: "127.0.0.1:50051"
rpc_addr: "127.0.0.1:50052"
log_level: "debug"
```

The broker will use `InMemoryStorage` with no replication. All data is lost on restart.

---

## Client Configuration

Used by producer and consumer CLIs. The file contains three sections: `broker`, `producer`, and `consumer`.

### Broker Connection

```yaml
broker:
  # gRPC endpoint of the broker (or load balancer in cluster mode).
  address: "http://localhost:50051"

  # Request timeout in seconds.
  timeout_secs: 30

  # Number of times to retry a failed request before giving up.
  max_retries: 3
```

### Producer Configuration

```yaml
producer:
  # Topic to publish messages to.
  topic: "events"

  # Target partition for all messages.
  # Key-based partitioning is not yet automatic; specify the partition explicitly.
  partition: 0

  # How many broker replicas must acknowledge a write.
  # -1 = all replicas (highest durability)
  #  0 = no acknowledgment (fire-and-forget)
  #  1 = leader only (default)
  required_acks: 1

  # Produce request timeout at the broker side (milliseconds).
  timeout_ms: 5000

  # Maximum number of messages to accumulate before forcing a flush.
  batch_size: 100

  # Maximum time to wait before flushing a non-full batch (milliseconds).
  # Lower values reduce latency; higher values improve throughput.
  flush_interval_ms: 100
```

### Consumer Configuration

```yaml
consumer:
  # Topic to consume from.
  topic: "events"

  # Partitions to consume from.
  partitions: [0]

  # Consumer group ID. Used for offset persistence and group coordination.
  group_id: "my-consumer-group"

  # Starting offset:
  #   -2 = earliest (read from the beginning of the partition)
  #   -1 = latest (only read messages written after startup)
  #   0+ = resume from a specific offset
  # If the broker has a committed offset for this group/topic/partition,
  # that committed offset takes precedence over this setting.
  offset: -2

  # Maximum bytes to fetch per request.
  max_bytes: 1048576   # 1 MiB

  # Maximum time the broker will wait before responding, even if fewer
  # than min_bytes are available (milliseconds).
  max_wait_ms: 1000

  # Minimum bytes the broker should accumulate before responding.
  # Setting this above 1 introduces broker-side wait time (reduces poll rate).
  min_bytes: 1

  # Automatically commit consumed offsets to the broker.
  auto_commit: true

  # How often to commit offsets when auto_commit is enabled (milliseconds).
  auto_commit_interval_ms: 5000

  # How often the consumer polls the broker for new messages (milliseconds).
  poll_interval_ms: 1000
```

---

## Full Example

See `config/example-full.yaml` in the repository for a single file showing all options with their defaults.

---

## Environment Variables

Logging verbosity can be overridden at runtime without changing the config file:

```bash
RUST_LOG=debug cargo run -- --mode broker
RUST_LOG=rust_mq=trace cargo run -- --mode consumer --config config/consumer.yaml
```

Valid values: `error`, `warn`, `info`, `debug`, `trace`.
