## Examples, Benchmarks, and Scripted Operations

### Purpose

Document executable operational surfaces used for local demos, throughput benchmarking, and cluster process management.

### Scope

**In scope:**
- Example binaries in [`examples/producer_consumer.rs`](../examples/producer_consumer.rs) and [`examples/benchmark.rs`](../examples/benchmark.rs).
- Criterion bench in [`benches/throughput.rs`](../benches/throughput.rs).
- Shell scripts in `scripts/` for cluster start/stop/failover test.

**Out of scope:**
- Production-grade deployment orchestration.
- Internal broker/client implementation details already covered elsewhere.

### Primary User Flow

1. Developer runs `cargo run --example producer_consumer` to validate end-to-end flow.
2. Developer runs `cargo run --example benchmark` (optional `BROKER_ADDR`) for latency/throughput experiments.
3. Developer runs `cargo bench` for criterion throughput benchmarks.
4. Operator uses `scripts/start-cluster.sh`, `scripts/stop-cluster.sh`, and `scripts/test-failover.sh` for local multi-node exercises.

### System Flow

1. `examples/producer_consumer.rs`:
- Starts embedded broker core and gRPC server.
- Starts producer and consumer.
- Sends demo messages, then graceful shutdown.
2. `examples/benchmark.rs`:
- Optionally starts embedded broker.
- Runs producer throughput, batch-size, end-to-end latency, and consumer throughput scenarios.
3. `benches/throughput.rs`:
- Starts singleton benchmark broker once (`Once` + dedicated runtime thread).
- Executes criterion benchmark groups for throughput and latency.
4. Scripts:
- `start-cluster.sh` builds release binary, launches 3 brokers with config files, stores PIDs.
- `stop-cluster.sh` kills stored PIDs.
- `test-failover.sh` starts producer+consumer and guides manual leader kill test.

### Data Model

- Example structs:
- `DemoPayload { sequence: u32, content: String, source: String }`.
- `CountingHandler` and `BenchmarkHandler` message handlers with atomic counters.
- Script state:
- PID files: `data/broker-1.pid`, `data/broker-2.pid`, `data/broker-3.pid`.
- Logs directory: `logs/broker-*.log`.

Persistence behavior:
- Scripts and examples write runtime logs/PID files under repository directories.

### Interfaces and Contracts

- Commands:
- `cargo run --example producer_consumer`
- `cargo run --example benchmark`
- `cargo bench`
- `./scripts/start-cluster.sh`, `./scripts/stop-cluster.sh`, `./scripts/test-failover.sh`
- Environment override:
- `BROKER_ADDR` in `examples/benchmark.rs` to target external broker.

### Dependencies

**Internal modules:**
- Public library exports from `rust_mq::{broker, client, codec}`.

**External services/libraries:**
- `criterion` for benchmarks.
- Shell utilities (`bash`, `kill`, `tail`) for scripts.

### Failure Modes and Edge Cases

- Scripts assume release binary path `./target/release/Rust-MQ` exists after build.
- Stale PID files can cause incorrect process kills.
- Benchmark latencies include client-side polling delays and are not pure broker microbenchmarks.

### Observability and Debugging

- Examples print to stdout and use logger outputs.
- Cluster scripts direct broker logs to `logs/broker-N.log`.
- Failover script suggests `grep 'leader' logs/broker-*.log` for leader identification.

### Risks and Notes

- These artifacts are developer tooling, not hardened production operations.
- Scripted cluster lifecycle has no health-check gate before dependent actions.

Changes:

