## Broker Node Config Contract

### Purpose

Define the YAML schema and defaults for starting a broker node in single-node dev mode, multi-node bootstrap, or join-existing-cluster mode.

### Scope

**In scope:**
- `BrokerConfig`, `ClusterConfig`, `RaftConfig`, and `RetentionConfig` in [`src/broker/config.rs`](../src/broker/config.rs).
- File loading via `BrokerConfig::from_file`.
- Runtime consumption by `run_broker`/`run_kraft_cluster` in [`src/main.rs`](../src/main.rs).

**Out of scope:**
- Client producer/consumer YAML schema (`src/client/config.rs`).
- Validation of topic-level business rules.

### Primary User Flow

1. Operator writes broker YAML (for example `config/broker-1.yaml`).
2. Operator starts `Rust-MQ --mode broker --config <file>`.
3. Config is deserialized into `BrokerConfig`.
4. Runtime selects transport (`grpc` or `sbe_tcp`), storage path, and cluster behavior from config fields.

### System Flow

1. `run_broker` receives `Args.config` and calls `BrokerConfig::from_file`.
2. `serde_yaml` maps YAML into typed structs with defaults for missing fields.
3. `run_kraft_cluster` reads transport, peer list, join address, raft tuning, and replication factor.
4. Broker starts controller and API servers using configured addresses.

### Data Model

- `BrokerConfig` fields:
- `node_id (u64)`
- `api_addr (String)`
- `rpc_addr (String)`
- `storage_path (String)`
- `cluster (Option<ClusterConfig>)`
- `raft (Option<RaftConfig>)`
- `retention (RetentionConfig)`
- `log_level (String)`
- `transport (String)`
- `join_addr (Option<String>)`
- `default_replication_factor (u16)`
- `ClusterConfig`: `initial_members (Vec<ClusterMember>)`, `bootstrap (bool)`.
- `ClusterMember`: `node_id`, `api_addr`, `rpc_addr`, `sbe_tcp_addr`.
- `RaftConfig`: heartbeat/election/snapshot and `rebalance_timeout_ms`.
- `RetentionConfig`: `retention_ms`, `max_messages_per_partition`.

Persistence behavior:
- YAML files are source-of-truth configuration.
- Runtime state persists separately in `storage_path` (sled), not back into YAML.

### Interfaces and Contracts

- Loader API: `BrokerConfig::from_file(path: &str) -> anyhow::Result<Self>`.
- Default API: `BrokerConfig::default_single_node()`.
- Mode check: `BrokerConfig::is_cluster_mode()`.
- Example contracts: [`config/example-full.yaml`](../config/example-full.yaml), [`config/broker-1.yaml`](../config/broker-1.yaml), [`config/broker-2.yaml`](../config/broker-2.yaml), [`config/broker-3.yaml`](../config/broker-3.yaml).

### Dependencies

**Internal modules:**
- [`src/main.rs`](../src/main.rs) consumes these settings to build the runtime.

**External services/libraries:**
- `serde`/`serde_yaml` for deserialization.

### Failure Modes and Edge Cases

- File read or YAML parse failure bubbles as `anyhow::Error` and prevents startup.
- `transport` is a free-form string; unknown values fall into the gRPC branch in `run_kraft_cluster` unless equal to `"sbe_tcp"`.
- No strong semantic validation in this module (for example incompatible cluster topology) before runtime actions begin.

### Observability and Debugging

- `run_broker` logs selected node id, API address, and storage path after config load.
- Debugging bad config starts at [`src/broker/config.rs`](../src/broker/config.rs) and YAML files under `config/`.

### Risks and Notes

- Schema evolution has no explicit migration mechanism; older YAML may parse but behave differently under new runtime assumptions.
- `default_replication_factor` clamping occurs downstream, not in config parsing.

Changes:

