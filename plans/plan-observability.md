# Feature: Observability

## 1. Purpose

The Observability module provides structured logging and metrics for every component in Rust-MQ. It initializes the logging framework at startup, defines log conventions for each subsystem, and exposes a Prometheus-compatible metrics registry that other modules can register counters, gauges, and histograms with.

Good observability is what makes the difference between "it seems to work" and "I know it works." Without it, diagnosing production issues, validating benchmark results, or verifying cluster health requires guesswork.

## 2. Responsibilities

- Initialize `env_logger` (or `tracing_subscriber`) from the `log_level` in `BrokerConfig` / `RUST_LOG` env var
- Define a standard set of log fields: `module`, `node_id`, `topic`, `partition`, `offset`, `group_id`
- Define Prometheus metric names and labels for broker, producer, and consumer operations
- Expose a `/metrics` HTTP endpoint on a configurable port returning Prometheus text format
- Provide a `Metrics` struct holding all registered metric handles, injected into components at construction
- Register per-component metrics: counters for messages produced/consumed, histograms for request latency, gauges for partition lag

## 3. Non-Responsibilities

- Does not ship logs to an external system (Loki, Elasticsearch, etc.)
- Does not implement distributed tracing (OpenTelemetry planned for a future release)
- Does not alert (alerting rules live in Prometheus/Alertmanager)
- Does not implement health check endpoints (planned for the CLI/deployment feature)
- Does not collect OS-level metrics (CPU, memory) — those are collected by the node exporter

## 4. Architecture Design

```
main.rs (startup)
    |
    | init_logging(log_level)
    | let metrics = Metrics::new()
    |
    +--> BrokerCore::new(..., metrics.broker.clone())
    +--> Producer::new(..., metrics.producer.clone())
    +--> Consumer::new(..., metrics.consumer.clone())
    +--> tokio::spawn(metrics_server(metrics_port, registry))

Each component records events:
    BrokerCore       → metrics.broker.produce_total.inc()
    Producer         → metrics.producer.batch_flush_total.inc()
    Consumer         → metrics.consumer.lag.set(latest - current)

                    Prometheus scrape
                    GET :9100/metrics
                    ← text/plain exposition format
```

## 5. Core Data Structures (Rust)

```rust
// src/observability.rs  (or src/obs.rs)

use prometheus::{
    Counter, CounterVec, Gauge, GaugeVec, Histogram, HistogramVec,
    Registry, TextEncoder,
};

/// All metrics for the broker server side.
#[derive(Clone)]
pub struct BrokerMetrics {
    /// Total messages produced successfully, labeled by topic.
    pub produce_total: CounterVec,
    /// Total produce requests rejected (not leader, unknown topic, etc.).
    pub produce_errors_total: CounterVec,
    /// Total messages fetched, labeled by topic.
    pub fetch_total: CounterVec,
    /// End-to-end produce request latency (seconds), labeled by topic.
    pub produce_latency: HistogramVec,
    /// End-to-end fetch request latency (seconds), labeled by topic.
    pub fetch_latency: HistogramVec,
    /// Whether this node is currently the Raft leader (1 = yes, 0 = no).
    pub is_leader: Gauge,
    /// Current Raft term.
    pub raft_term: Gauge,
}

/// All metrics for the producer client.
#[derive(Clone)]
pub struct ProducerMetrics {
    /// Total messages buffered in the batch.
    pub batch_buffered_total: Counter,
    /// Total batch flush operations (timer or size trigger).
    pub batch_flush_total: Counter,
    /// Total messages sent to the broker successfully.
    pub messages_sent_total: Counter,
    /// Total send errors.
    pub send_errors_total: Counter,
    /// Number of messages currently in the batch buffer.
    pub batch_size: Gauge,
}

/// All metrics for the consumer client.
#[derive(Clone)]
pub struct ConsumerMetrics {
    /// Total messages delivered to the handler.
    pub messages_consumed_total: Counter,
    /// Total handler errors.
    pub handler_errors_total: Counter,
    /// Total offset commit operations.
    pub commits_total: Counter,
    /// Consumer lag: latest broker offset minus current consumer offset.
    pub lag: Gauge,
}

/// Top-level metrics container. Clone-able; all clones share the same underlying registry.
#[derive(Clone)]
pub struct Metrics {
    pub broker:   BrokerMetrics,
    pub producer: ProducerMetrics,
    pub consumer: ConsumerMetrics,
    registry:     Arc<Registry>,
}
```

## 6. Public Interfaces

```rust
/// Initialize the logging framework from the given level string.
/// Must be called once, at the start of main().
pub fn init_logging(level: &str);

impl Metrics {
    /// Create a new Metrics instance and register all metrics with a fresh Registry.
    pub fn new() -> Self;

    /// Create Metrics using an existing Registry (useful for testing).
    pub fn with_registry(registry: Arc<Registry>) -> Self;

    /// Gather all metrics and encode them as Prometheus text format.
    pub fn encode(&self) -> anyhow::Result<String>;
}

/// Start an HTTP server that serves Prometheus metrics at GET /metrics.
/// addr: "host:port", e.g. "0.0.0.0:9100"
pub async fn run_metrics_server(addr: SocketAddr, metrics: Metrics) -> anyhow::Result<()>;
```

## 7. Internal Algorithms

### init_logging

```
init_logging(level):
  let filter = EnvFilter::try_from_default_env()
               .unwrap_or_else(|_| EnvFilter::new(level))
  tracing_subscriber::fmt()
      .with_env_filter(filter)
      .with_target(true)    // include module path
      .with_thread_ids(false)
      .compact()
      .init()
```

### Metrics::new

```
Metrics::new():
  registry = Registry::new()

  broker = BrokerMetrics {
    produce_total: register CounterVec with labels ["topic"] in registry,
    produce_errors_total: register CounterVec with labels ["topic", "error"] in registry,
    fetch_total: register CounterVec with labels ["topic"] in registry,
    produce_latency: register HistogramVec with labels ["topic"],
                     buckets: [0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0],
    fetch_latency: same buckets,
    is_leader: register Gauge,
    raft_term: register Gauge,
  }

  producer = ProducerMetrics { ... }
  consumer = ConsumerMetrics { ... }

  return Metrics { broker, producer, consumer, registry: Arc::new(registry) }
```

### run_metrics_server

```
run_metrics_server(addr, metrics):
  listener = TcpListener::bind(addr).await?
  loop:
    (stream, _) = listener.accept().await?
    tokio::spawn(async move {
      // Minimal HTTP response: only handle GET /metrics
      let body = metrics.encode().unwrap_or_default()
      write HTTP 200 response with Content-Type: text/plain; version=0.0.4
      write body
    })
```

## 8. Persistence Model

Metrics are in-memory accumulators. All counters reset to zero on process restart. Persistent metric history requires a Prometheus server scraping the endpoint and storing time series.

## 9. Concurrency Model

| Object | Primitive | Usage |
|---|---|---|
| `Metrics` | `Clone` (Arc-shared registry) | All clones share the same prometheus::Registry; `CounterVec` / `Gauge` are thread-safe internally |
| Metrics server | `tokio::spawn` per connection | Stateless; each request encodes the current registry state |

Prometheus counters use `Arc<AtomicF64>` internally and require no external locking.

## 10. Configuration

```rust
pub struct ObservabilityConfig {
    /// Log level: "error", "warn", "info", "debug", "trace".
    pub log_level: String,
    /// Address for the Prometheus metrics HTTP server. None = disabled.
    pub metrics_addr: Option<SocketAddr>,
}
```

Defaults: `log_level = "info"`, `metrics_addr = None` (metrics server is opt-in).

## 11. Observability

The observability module observes itself only for initialization errors (logged to stderr before the logger is initialized). No self-referential metrics.

## 12. Testing Strategy

**Unit tests**:
- `test_init_logging_does_not_panic`: call `init_logging("debug")`, assert no panic
- `test_metrics_new_registers_all`: `Metrics::new()`, encode output, assert all metric names present in text
- `test_counter_increments`: call `metrics.broker.produce_total.with_label_values(&["events"]).inc()`, encode, assert value = 1
- `test_gauge_sets`: call `metrics.consumer.lag.set(42.0)`, encode, assert value = 42
- `test_histogram_observes`: call `metrics.broker.produce_latency.with_label_values(&["events"]).observe(0.01)`, encode, assert count = 1
- `test_encode_returns_valid_text`: encode output starts with `# HELP` and `# TYPE` headers
- `test_metrics_server_serves_metrics`: start server on a random port, send GET /metrics, assert 200 response with metric names

## 13. Open Questions

None.
