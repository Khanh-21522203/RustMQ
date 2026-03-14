# Feature: CLI

## 1. Purpose

The CLI is the entry point (`src/main.rs`) that wires all subsystems together and provides the operational surface for running Rust-MQ as a broker, a producer, or a consumer. It parses command-line arguments, loads the appropriate configuration file, initializes logging, instantiates subsystems, and manages graceful shutdown on `Ctrl+C`.

The CLI does not contain business logic — it exists solely to compose the library components in the right order and hand them the right configuration.

## 2. Responsibilities

- Parse CLI flags using `clap`: `--mode {broker|producer|consumer}`, `--config <path>`, `--broker <addr>`
- Load and validate the correct config file for the selected mode
- Initialize the logging framework via the observability module
- For `--mode broker`: instantiate storage (`InMemoryStorage` or `MultiBroker`), `BrokerCore`, `KafkaBrokerServer`, and start the gRPC server
- For `--mode producer`: connect `KafkaBrokerClient`, create `Producer`, run an interactive stdin read loop, flush and shut down on `Ctrl+C`
- For `--mode consumer`: connect `KafkaBrokerClient`, create `Consumer`, register a `PrintHandler`, start the poll loop, shut down on `Ctrl+C`
- Install a `Ctrl+C` signal handler (`tokio::signal::ctrl_c()`) and initiate graceful shutdown for all modes
- Print clear startup and shutdown messages to stdout

## 3. Non-Responsibilities

- Does not implement business logic (message routing, offset resolution, Raft)
- Does not parse message payloads (treated as raw strings from stdin for the producer CLI)
- Does not implement clustering setup beyond what `MultiBroker::new` handles
- Does not expose an HTTP admin API (planned for a future release)

## 4. Architecture Design

```
$ cargo run -- --mode broker --config config/broker-1.yaml

main()
    |
    | parse_args() → Mode::Broker { config_path }
    | load_broker_config(config_path) → BrokerConfig
    | init_logging(config.log_level)
    |
    v
run_broker(BrokerConfig)
    |
    +--> if config.cluster.is_some():
    |      MultiBroker::new(config).await? → storage
    |    else:
    |      InMemoryStorage::new() → storage
    |
    +--> let (tx, rx) = mpsc::unbounded_channel()
    +--> let core = BrokerCore::new(storage, rx)
    +--> let server = KafkaBrokerServer::new(tx)
    +--> tokio::spawn(core.run())
    +--> tokio::spawn(server.serve(api_addr))
    +--> tokio::signal::ctrl_c().await   // block until shutdown
    +--> log "broker shutting down"

$ cargo run -- --mode producer --config config/producer.yaml

main()
    → run_producer(AppConfig)
        → KafkaBrokerClient::connect(config.broker.address)
        → Producer::new(client, config.producer)
        → loop: read stdin line → producer.send(line.into_bytes()) until EOF or Ctrl+C
        → producer.shutdown()

$ cargo run -- --mode consumer --config config/consumer.yaml

main()
    → run_consumer(AppConfig)
        → KafkaBrokerClient::connect(config.broker.address)
        → Consumer::new(client, config.consumer)
        → consumer.start(PrintHandler).await
        → tokio::signal::ctrl_c().await
        → consumer.shutdown()
```

## 5. Core Data Structures (Rust)

```rust
// src/main.rs

use clap::Parser;

/// Kafka-inspired message queue in Rust.
#[derive(Parser, Debug)]
#[command(name = "rust-mq", version, about)]
struct Args {
    /// Operating mode: broker, producer, or consumer.
    #[arg(long, value_enum)]
    mode: Mode,

    /// Path to the YAML configuration file.
    #[arg(long, default_value = "config/default.yaml")]
    config: PathBuf,

    /// Broker address override (overrides broker.address in the config file).
    /// Format: "http://host:port"
    #[arg(long)]
    broker: Option<String>,
}

#[derive(clap::ValueEnum, Clone, Debug)]
enum Mode {
    Broker,
    Producer,
    Consumer,
}

/// Built-in message handler for the consumer CLI: prints each message to stdout.
struct PrintHandler;

#[async_trait]
impl MessageHandler for PrintHandler {
    async fn handle(&self, msg: ConsumedMessage) -> anyhow::Result<()> {
        println!(
            "[topic={} partition={} offset={}] {}",
            msg.topic,
            msg.partition,
            msg.offset,
            String::from_utf8_lossy(&msg.value),
        );
        Ok(())
    }
}
```

## 6. Public Interfaces

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()>;

async fn run_broker(config: BrokerConfig) -> anyhow::Result<()>;
async fn run_producer(config: AppConfig) -> anyhow::Result<()>;
async fn run_consumer(config: AppConfig) -> anyhow::Result<()>;
```

These are `pub(crate)` in the final implementation; they are listed here to clarify structure.

## 7. Internal Algorithms

### main

```
main():
  args = Args::parse()

  match args.mode:
    Mode::Broker →
      cfg = load_broker_config(&args.config)?
      init_logging(&cfg.log_level)
      run_broker(cfg).await?

    Mode::Producer →
      cfg = load_app_config(&args.config)?
      if let Some(addr) = args.broker: cfg.broker.address = addr
      init_logging("info")
      run_producer(cfg).await?

    Mode::Consumer →
      cfg = load_app_config(&args.config)?
      if let Some(addr) = args.broker: cfg.broker.address = addr
      init_logging("info")
      run_consumer(cfg).await?
```

### run_broker

```
run_broker(config):
  let api_addr: SocketAddr = config.api_addr.parse()?

  let storage: Box<dyn BrokerStorage> =
    if config.cluster.is_some():
      Box::new(MultiBroker::new(config.clone()).await?)
    else:
      Box::new(InMemoryStorage::new())

  let (tx, rx) = mpsc::unbounded_channel()
  let core   = BrokerCore::new(storage, rx)
  let server = KafkaBrokerServer::new(tx)

  tokio::spawn(core.run())

  log::info!("broker listening on {api_addr}")
  tokio::select!:
    result = server.serve(api_addr) → result?
    _ = tokio::signal::ctrl_c()    → log::info!("shutting down")

  return Ok(())
```

### run_producer

```
run_producer(config):
  producer_cfg = config.producer.ok_or("no producer section in config")?
  client = KafkaBrokerClient::connect(&config.broker.address).await?
  producer = Producer::new(client, producer_cfg)

  log::info!("producer ready; type messages and press Enter. Ctrl+C to exit.")

  let stdin = tokio::io::BufReader::new(tokio::io::stdin())
  loop:
    tokio::select!:
      line = stdin.read_line() →
        match line:
          Ok(0) | Err(_) → break        // EOF
          Ok(_) → producer.send(ProducerMessage {
                    key: None,
                    value: line.trim().as_bytes().to_vec(),
                    partition: None,
                  }).await?
      _ = tokio::signal::ctrl_c() → break

  producer.shutdown().await?
  log::info!("producer shut down")
  return Ok(())
```

### run_consumer

```
run_consumer(config):
  consumer_cfg = config.consumer.ok_or("no consumer section in config")?
  client = KafkaBrokerClient::connect(&config.broker.address).await?
  mut consumer = Consumer::new(client, consumer_cfg)

  consumer.start(PrintHandler).await?
  log::info!("consumer started; press Ctrl+C to stop")

  tokio::signal::ctrl_c().await?
  consumer.shutdown().await?
  log::info!("consumer shut down")
  return Ok(())
```

## 8. Persistence Model

The CLI itself is stateless. All persistence is handled by the storage backend (`MultiBroker` → Raft log) or the broker's offset store (consumer group commits).

## 9. Concurrency Model

| Object | Primitive | Usage |
|---|---|---|
| `BrokerCore` task | `tokio::spawn` | Independent task; runs until mpsc sender dropped |
| `KafkaBrokerServer` task | `tokio::spawn` inside `serve()` | Tonic manages one task per active RPC |
| `Consumer` poll task | `tokio::spawn` inside `start()` | Stopped via shutdown signal |
| Shutdown coordination | `tokio::signal::ctrl_c()` or `tokio::select!` | Main task awaits signal and then calls shutdown methods |

## 10. Configuration

The CLI itself has no configuration beyond the `--mode`, `--config`, and `--broker` flags. All tuning is in the YAML config files (see `plan-configuration.md`).

| Flag | Default | Description |
|---|---|---|
| `--mode` | (required) | `broker`, `producer`, or `consumer` |
| `--config` | `config/default.yaml` | Path to YAML config file |
| `--broker` | (none) | Override broker address (client modes only) |

## 11. Observability

- Every mode prints a startup line at `INFO`: `"broker listening on {addr}"` / `"producer ready"` / `"consumer started"`
- Every graceful shutdown prints `"shutting down"` and `"shut down"` at `INFO`
- Config load errors printed at `ERROR` with the file path
- Connection failures in producer/consumer modes printed at `ERROR` before exiting

## 12. Testing Strategy

**Unit tests** (no network, mock storage):
- `test_run_broker_single_node`: starts `run_broker` with no cluster config, sends a produce request via `KafkaBrokerClient`, asserts success
- `test_broker_mode_uses_inmemory_without_cluster`: assert `InMemoryStorage` is used when no `cluster` section is in config
- `test_broker_mode_uses_multiBroker_with_cluster`: assert `MultiBroker::new` is called when cluster config is present (using a spy/mock)

**Integration tests** (real process, loopback network):
- `test_producer_cli_sends_stdin_line`: spawn producer process, write a line to stdin pipe, assert broker received message
- `test_consumer_cli_prints_messages`: produce message via API, spawn consumer CLI, assert expected output in stdout
- `test_ctrl_c_triggers_graceful_shutdown`: spawn broker, send SIGINT, assert process exits cleanly within 5 seconds

## 13. Open Questions

None.
