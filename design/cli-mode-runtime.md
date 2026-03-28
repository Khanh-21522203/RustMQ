## CLI Mode Runtime

### Purpose

Provide one executable entrypoint (`rust-mq`) that can run as a broker, producer, or consumer from the same binary.

### Scope

**In scope:**
- Parse `--mode`, `--config`, and `--broker` CLI arguments in [`src/main.rs`](../src/main.rs).
- Initialize logging with mode-aware defaults (`init_logger`).
- Dispatch to `run_broker`, `run_producer`, or `run_consumer`.
- Handle Ctrl+C driven shutdown for all three modes.

**Out of scope:**
- Broker request handling internals (`src/broker/server/*`).
- Producer/consumer protocol details (`src/client/*`).
- Raft transport encoding (`src/broker/grpc/*`, `src/broker/sbe_tcp/*`).

### Primary User Flow

1. Operator runs `Rust-MQ --mode broker --config config/broker-1.yaml` or a producer/consumer command.
2. Process parses args and initializes logger.
3. Runtime enters the selected mode path.
4. Mode keeps running until Ctrl+C, then performs graceful shutdown where implemented.

### System Flow

1. Entry point: [`main`](../src/main.rs) parses `Args` (`clap::Parser`).
2. Logger setup: `init_logger` optionally loads broker config to set `log_level`.
3. Mode dispatch:
- `Mode::Broker` -> `run_broker`.
- `Mode::Producer` -> `run_producer`.
- `Mode::Consumer` -> `run_consumer`.
4. Broker path chooses in-memory single-node (`BrokerConfig::default_single_node`) when no config, or KRaft cluster (`run_kraft_cluster`) when config exists.
5. Producer/consumer paths load `AppConfig` from YAML or defaults, validate, then start client runtime.

```text
CLI
  -> main (src/main.rs)
     -> run_broker
        -> in-memory mode (no config)
        -> KRaft mode (config present)
     -> run_producer
     -> run_consumer
```

### Data Model

- `Args` - fields: `mode (Mode)`, `config (Option<String>)`, `broker (Option<String>)`.
- `Mode` - variants: `Broker`, `Producer`, `Consumer`.
- `PrintHandler` - stateless consumer `MessageHandler` implementation that formats consumed records for stdout.

Persistence behavior:
- CLI arguments are ephemeral.
- Selected mode may open persistent storage via downstream components (`sled` paths in broker mode).

### Interfaces and Contracts

- CLI contract (`clap`):
- `--mode <broker|producer|consumer>` is required.
- `--config <path>` optional YAML path.
- `--broker <host:port or URL>` optional override for producer/consumer target.
- Broker mode contract:
- With `--config`, KRaft startup is used.
- Without `--config`, in-memory dev broker is used.

### Dependencies

**Internal modules:**
- [`src/broker/config.rs`](../src/broker/config.rs) for broker config parsing/defaults.
- [`src/client/config.rs`](../src/client/config.rs) for producer/consumer config parsing.
- [`src/client/producer.rs`](../src/client/producer.rs), [`src/client/consumer.rs`](../src/client/consumer.rs) for runtime clients.

**External services/libraries:**
- `clap` for argument parsing.
- `tokio` for async runtime and signal handling.
- `env_logger`/`log` for logging.

### Failure Modes and Edge Cases

- Missing producer section in config: `run_producer` returns `Producer configuration not found in config file`.
- Missing consumer section in config: `run_consumer` returns `Consumer configuration not found in config file`.
- Invalid `host:port` in helper `parse_host_port` falls back to `("localhost", 50051)` silently.
- `run_broker` in in-memory mode only logs server startup errors from spawned tasks; task panics are not joined.

### Observability and Debugging

- Startup logs include selected mode and addresses (`run_broker`/`run_producer`/`run_consumer`).
- Ctrl+C is the single shutdown trigger path in CLI mode.
- First place to debug mode selection or startup failures: [`src/main.rs`](../src/main.rs).

### Risks and Notes

- Silent fallback in `parse_host_port` can hide malformed addresses.
- Producer `send` path logs queued/sent semantics from downstream producer, not guaranteed end-to-end processing.
- Mode branching in one file makes `main.rs` large (500+ lines), so regression risk is concentrated.

Changes:
