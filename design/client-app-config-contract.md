## Client App Config Contract

### Purpose

Define the producer/consumer YAML schema, default values, and validation rules for CLI and library client startup.

### Scope

**In scope:**
- `AppConfig`, `BrokerConfig`, `ProducerConfig`, and `ConsumerConfig` in [`src/client/config.rs`](../src/client/config.rs).
- Config loading/validation flow used by `run_producer` and `run_consumer` in [`src/main.rs`](../src/main.rs).

**Out of scope:**
- Broker node YAML schema (`src/broker/config.rs`).
- Runtime producer/consumer loop behavior.

### Primary User Flow

1. User provides `config/producer.yaml` or `config/consumer.yaml`.
2. CLI mode loads YAML via `AppConfig::from_file`.
3. Config is validated (`AppConfig::validate`).
4. Mode extracts producer/consumer section and starts runtime.

### System Flow

1. `run_producer`/`run_consumer` choose between file-based config and `default_producer`/`default_consumer`.
2. `AppConfig::validate` checks non-empty topic and allowed topic-name characters.
3. Mode-specific section is required (`producer` or `consumer`), else return error.

### Data Model

- `AppConfig { broker: BrokerConfig, producer: Option<ProducerConfig>, consumer: Option<ConsumerConfig> }`.
- Client `BrokerConfig { address: String, timeout_secs: u64, max_retries: u32 }`.
- `ProducerConfig`:
- `topic`, `partition`, `partitioning`, `num_partitions`, `required_acks`, `timeout_ms`, `batch_size`, `flush_interval_ms`.
- `ConsumerConfig`:
- `topic`, `partitions`, `group_id`, `offset`, `max_bytes`, `max_wait_ms`, `min_bytes`, `auto_commit`, `auto_commit_interval_ms`, `poll_interval_ms`.

Persistence behavior:
- YAML config files are static input; runtime does not mutate or persist config.

### Interfaces and Contracts

- Public APIs:
- `AppConfig::from_file`, `from_yaml_str`, `default_producer`, `default_consumer`, `validate`.
- Topic validation contract:
- Non-empty.
- Characters limited to ASCII alphanumeric plus `_`, `.`, `-`.

### Dependencies

**Internal modules:**
- `main.rs` mode handlers consume parsed config.

**External services/libraries:**
- `serde`/`serde_yaml` for config parsing.
- `anyhow` for validation/loading errors.

### Failure Modes and Edge Cases

- Invalid YAML/file path returns contextual `anyhow` error.
- Unknown `partitioning` strings pass validation and fall back later in producer logic.
- Validation does not enforce all semantic ranges (for example `num_partitions > 0` or non-negative timeout fields).

### Observability and Debugging

- CLI warns when config file is omitted and defaults are used.
- Debug config issues by checking parsed struct values and `validate` return path in [`src/client/config.rs`](../src/client/config.rs).

### Risks and Notes

- Minimal validation in config module shifts many checks to runtime behavior.
- Broker retry/timeouts fields are declared but not uniformly enforced by all client code paths.

Changes:

