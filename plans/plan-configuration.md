# Feature: Configuration

## 1. Purpose

The Configuration module defines all YAML-deserializable config structs for every component in Rust-MQ. It provides a single source of truth for what every knob does, what its default value is, and how it is validated at startup. All other modules receive their configuration as strongly-typed structs — no string maps, no environment variable parsing outside this module.

## 2. Responsibilities

- Define `BrokerConfig`: broker node identity, cluster membership, Raft tuning, and log level
- Define `AppConfig`: the top-level client config containing `BrokerConnection`, `ProducerConfig`, and `ConsumerConfig`
- Define `BrokerConnection`: the broker endpoint used by producers and consumers
- Define `ProducerConfig`: topic, partition, batch size, flush interval, ack level
- Define `ConsumerConfig`: topic, partition, group, offset sentinel, poll interval, auto-commit settings
- Implement `Default` for every config struct with production-safe defaults
- Provide `load_broker_config(path)` and `load_app_config(path)` that read YAML files and return typed configs
- Validate required fields and forbidden value combinations at load time, returning descriptive errors

## 3. Non-Responsibilities

- Does not read environment variables (no `RUST_MQ_*` env var overrides; planned for a future release)
- Does not hot-reload configuration at runtime
- Does not manage TLS certificates or secrets (planned for the security feature)
- Does not discover peers via DNS; all cluster members must be listed in `initial_members`

## 4. Architecture Design

```
CLI (main.rs)
    |
    | --config path.yaml
    v
+-------------------------------------------+
|          Configuration module             |
|  load_broker_config(path) → BrokerConfig  |
|  load_app_config(path)    → AppConfig     |
+-------------------------------------------+
    |
    | passes typed structs to:
    +--> run_broker(BrokerConfig)
    |       └─> MultiBroker::new(BrokerConfig)
    |       └─> KafkaBrokerServer::serve(api_addr)
    |
    +--> run_producer(AppConfig.broker, AppConfig.producer)
    |       └─> KafkaBrokerClient::connect(broker.address)
    |       └─> Producer::new(client, producer_config)
    |
    +--> run_consumer(AppConfig.broker, AppConfig.consumer)
            └─> KafkaBrokerClient::connect(broker.address)
            └─> Consumer::new(client, consumer_config)
```

## 5. Core Data Structures (Rust)

```rust
// src/broker/config.rs

/// Full configuration for one broker node.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct BrokerConfig {
    /// Unique identifier for this node. Must match an entry in cluster.initial_members.
    pub node_id: u64,

    /// Address this broker listens on for client gRPC connections.
    /// Format: "host:port", e.g. "0.0.0.0:9092".
    pub api_addr: String,

    /// Address this broker listens on for Raft peer-to-peer gRPC traffic.
    /// Must be reachable by all other nodes in the cluster.
    pub rpc_addr: String,

    /// Directory for persistent Raft state (log, snapshots).
    /// Optional: if absent, the broker uses in-memory storage (single-node mode).
    #[serde(default)]
    pub storage_path: Option<PathBuf>,

    /// Cluster membership config. Optional: if absent, single-node mode is used.
    #[serde(default)]
    pub cluster: Option<ClusterConfig>,

    /// Raft protocol tuning. Ignored in single-node mode.
    #[serde(default)]
    pub raft: RaftTuning,

    /// Log verbosity level: "error", "warn", "info", "debug", "trace".
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ClusterConfig {
    /// All nodes in the cluster including this node.
    pub initial_members: Vec<ClusterMember>,

    /// Set true only on the first node during initial cluster bring-up.
    /// Must be false (or omitted) on all nodes once the cluster is running.
    #[serde(default)]
    pub bootstrap: bool,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ClusterMember {
    pub node_id: u64,
    pub api_addr: String,
    pub rpc_addr: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct RaftTuning {
    /// How often the leader sends heartbeats (milliseconds).
    #[serde(default = "default_heartbeat_ms")]
    pub heartbeat_interval_ms: u64,

    /// Minimum follower election timeout (milliseconds).
    #[serde(default = "default_election_min_ms")]
    pub election_timeout_min_ms: u64,

    /// Maximum follower election timeout (milliseconds). Wider range reduces split votes.
    #[serde(default = "default_election_max_ms")]
    pub election_timeout_max_ms: u64,

    /// Raft log entries between snapshots.
    #[serde(default = "default_snapshot_threshold")]
    pub snapshot_threshold: u64,
}

impl Default for RaftTuning {
    fn default() -> Self {
        RaftTuning {
            heartbeat_interval_ms:   1000,
            election_timeout_min_ms: 3000,
            election_timeout_max_ms: 6000,
            snapshot_threshold:      10000,
        }
    }
}

// src/client/config.rs

/// Top-level client config file. Brokers, producers, and consumers each have a section.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct AppConfig {
    pub broker:   BrokerConnection,
    #[serde(default)]
    pub producer: Option<ProducerConfig>,
    #[serde(default)]
    pub consumer: Option<ConsumerConfig>,
}

/// Broker connection parameters used by both producer and consumer clients.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct BrokerConnection {
    /// gRPC endpoint of the broker. Format: "http://host:port".
    pub address: String,

    /// Per-request timeout (seconds).
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,

    /// Number of retries before returning an error.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ProducerConfig {
    pub topic:             String,
    #[serde(default)]
    pub partition:         i32,
    /// -1 = all, 0 = none, 1 = leader only.
    #[serde(default = "default_required_acks")]
    pub required_acks:     i32,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms:        i32,
    #[serde(default = "default_batch_size")]
    pub batch_size:        usize,
    #[serde(default = "default_flush_interval_ms")]
    pub flush_interval_ms: u64,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ConsumerConfig {
    pub topic:                   String,
    #[serde(default)]
    pub partition:               i32,
    pub group_id:                String,
    /// -2 = earliest, -1 = latest, 0+ = specific offset.
    #[serde(default = "default_consumer_offset")]
    pub offset:                  i64,
    #[serde(default = "default_max_bytes")]
    pub max_bytes:               i32,
    #[serde(default = "default_max_wait_ms")]
    pub max_wait_ms:             i32,
    #[serde(default = "default_min_bytes")]
    pub min_bytes:               i32,
    #[serde(default = "default_auto_commit")]
    pub auto_commit:             bool,
    #[serde(default = "default_auto_commit_interval_ms")]
    pub auto_commit_interval_ms: u64,
    #[serde(default = "default_poll_interval_ms")]
    pub poll_interval_ms:        u64,
}
```

## 6. Public Interfaces

```rust
/// Load and validate a BrokerConfig from a YAML file.
/// Returns a descriptive error if the file is missing, unparseable,
/// or contains invalid values (e.g. election_timeout_min > max).
pub fn load_broker_config(path: &Path) -> anyhow::Result<BrokerConfig>;

/// Load and validate an AppConfig from a YAML file.
pub fn load_app_config(path: &Path) -> anyhow::Result<AppConfig>;

/// Validation helpers (exposed for use in tests)
pub fn validate_broker_config(cfg: &BrokerConfig) -> anyhow::Result<()>;
pub fn validate_app_config(cfg: &AppConfig) -> anyhow::Result<()>;
```

## 7. Internal Algorithms

### load_broker_config

```
load_broker_config(path):
  bytes = std::fs::read(path)?
  cfg: BrokerConfig = serde_yaml::from_slice(&bytes)?
  validate_broker_config(&cfg)?
  return Ok(cfg)
```

### validate_broker_config

```
validate_broker_config(cfg):
  if cfg.api_addr is empty: return Err("api_addr is required")
  if cfg.rpc_addr is empty: return Err("rpc_addr is required")

  if cfg.cluster is Some(cluster):
    if cluster.initial_members is empty:
      return Err("cluster.initial_members must not be empty")
    if !cluster.initial_members.any(|m| m.node_id == cfg.node_id):
      return Err("node_id must appear in cluster.initial_members")
    if cfg.raft.election_timeout_min_ms >= cfg.raft.election_timeout_max_ms:
      return Err("election_timeout_min_ms must be less than election_timeout_max_ms")
    if cfg.raft.heartbeat_interval_ms >= cfg.raft.election_timeout_min_ms:
      return Err("heartbeat_interval_ms should be less than election_timeout_min_ms")
    bootstrap_count = cluster.initial_members.count(|m| m.bootstrap)
    // bootstrap field is on the ClusterConfig, not per-member; just note if set
  return Ok(())
```

### validate_app_config

```
validate_app_config(cfg):
  if cfg.broker.address is empty: return Err("broker.address is required")

  if cfg.producer is Some(p):
    if p.topic is empty: return Err("producer.topic is required")
    if p.batch_size == 0: return Err("producer.batch_size must be > 0")
    if p.flush_interval_ms == 0: return Err("producer.flush_interval_ms must be > 0")

  if cfg.consumer is Some(c):
    if c.topic is empty: return Err("consumer.topic is required")
    if c.group_id is empty: return Err("consumer.group_id is required")
    if c.offset < -2: return Err("consumer.offset must be >= -2")
  return Ok(())
```

## 8. Persistence Model

Configuration is read from disk once at startup and held in memory for the lifetime of the process. There is no runtime reload. Changes require a process restart.

## 9. Concurrency Model

All config structs are `Clone + Send + Sync`. They are loaded by the main thread, then cloned into subsystem constructors. No locks are needed — config is immutable after loading.

## 10. Configuration

Configuration defaults:

| Field | Default |
|---|---|
| `broker.timeout_secs` | 30 |
| `broker.max_retries` | 3 |
| `producer.partition` | 0 |
| `producer.required_acks` | 1 |
| `producer.timeout_ms` | 5000 |
| `producer.batch_size` | 100 |
| `producer.flush_interval_ms` | 100 |
| `consumer.partition` | 0 |
| `consumer.offset` | -2 (earliest) |
| `consumer.max_bytes` | 1 048 576 (1 MiB) |
| `consumer.max_wait_ms` | 1000 |
| `consumer.min_bytes` | 1 |
| `consumer.auto_commit` | true |
| `consumer.auto_commit_interval_ms` | 5000 |
| `consumer.poll_interval_ms` | 1000 |
| `raft.heartbeat_interval_ms` | 1000 |
| `raft.election_timeout_min_ms` | 3000 |
| `raft.election_timeout_max_ms` | 6000 |
| `raft.snapshot_threshold` | 10000 |
| `log_level` | "info" |

## 11. Observability

- `load_broker_config`: `INFO` log with path and parsed `node_id` on success; `ERROR` with parse/validation error on failure
- `load_app_config`: `INFO` log with path on success; `ERROR` on failure
- All validation errors include the field name and the received value

## 12. Testing Strategy

**Unit tests**:
- `test_load_valid_broker_config`: parse sample broker YAML, assert all fields populated correctly
- `test_broker_config_defaults`: parse minimal YAML (node_id + api_addr + rpc_addr only), assert all optional fields have correct defaults
- `test_validate_rejects_missing_api_addr`: empty api_addr returns descriptive error
- `test_validate_rejects_node_not_in_members`: node_id not in initial_members returns error
- `test_validate_rejects_bad_election_timeout`: min >= max returns error
- `test_load_valid_app_config`: parse sample producer/consumer YAML, assert all fields
- `test_app_config_defaults`: parse minimal client YAML, assert defaults applied
- `test_validate_rejects_empty_topic`: consumer with empty topic returns error
- `test_validate_rejects_negative_offset`: consumer with offset = -3 returns error
- `test_load_nonexistent_file`: returns `Err` with descriptive message

## 13. Open Questions

None.
