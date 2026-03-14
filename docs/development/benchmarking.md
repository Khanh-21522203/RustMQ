# Benchmarking

Rust-MQ includes two benchmarking tools:

| Tool | Purpose |
|---|---|
| `examples/benchmark.rs` | End-to-end throughput and latency measurements |
| `benches/` (Criterion) | Statistical micro-benchmarks with regression detection |

## Running the Benchmark Example

```bash
cargo run --release --example benchmark
```

The benchmark measures:

1. **Throughput at different message sizes** — 100B, 1KB, 10KB payloads
2. **Batch size impact** — batch sizes of 10, 100, 1000
3. **End-to-end latency** — p50, p95, p99 percentiles

Example output:

```
=== Throughput Benchmark ===
Message size: 100 bytes
  Sent 100,000 messages in 124ms
  Throughput: 805,797 msg/s | 76.78 MB/s

Message size: 1024 bytes
  Sent 100,000 messages in 271ms
  Throughput: 368,992 msg/s | 351.29 MB/s

Message size: 10240 bytes
  Sent 10,000 messages in 143ms
  Throughput: 70,028 msg/s | 674.31 MB/s

=== Batch Size Impact ===
Batch size: 10    → 523,811 msg/s
Batch size: 100   → 1,234,568 msg/s
Batch size: 1000  → 2,105,263 msg/s

=== Latency Benchmark (1,000 sync sends) ===
  p50:  10.02ms
  p95:  11.91ms
  p99:  12.04ms
  max:  14.33ms
```

## Criterion Benchmarks

```bash
# Run all benchmarks
cargo bench

# Run with a filter
cargo bench -- produce

# View the HTML report
open target/criterion/report/index.html
```

Criterion runs each benchmark multiple times, applies statistical analysis, and detects performance regressions between runs. The HTML report includes throughput charts, distribution plots, and comparison with the previous run.

## Interpreting Results

### Throughput

Throughput scales with batch size because larger batches amortize the fixed cost of each gRPC round trip:

- **Small batches / `send_sync()`**: Latency-bound — each message waits for a broker round trip.
- **Large batches**: Throughput-bound — the bottleneck is serialization and network bandwidth.

### Latency

The latency benchmark calls `send_sync()` in a loop with no batching. This measures the pure round-trip time: serialization → gRPC → broker processing → response → deserialization.

Typical p99 is in the 10–15ms range on localhost. Over a real network, add the actual network RTT.

### Effect of `required_acks`

| Setting | Latency | Durability |
|---|---|---|
| `0` (no ack) | ~0ms additional | None — message may be lost |
| `1` (leader) | baseline | Message written to leader |
| `-1` (all) | +Raft replication time | Message written to majority |

In cluster mode, `required_acks: -1` adds the Raft replication round trip (~1–5ms on LAN).

## Profiling

To profile a benchmark with `perf` (Linux):

```bash
cargo build --release --example benchmark
perf record --call-graph dwarf ./target/release/examples/benchmark
perf report
```

Or use `flamegraph`:

```bash
cargo install flamegraph
cargo flamegraph --example benchmark
```

The resulting `flamegraph.svg` shows where CPU time is spent.
