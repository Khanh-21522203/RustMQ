# Feature: gRPC Transport

## 1. Purpose

The gRPC Transport layer is the network boundary of Rust-MQ. It defines the Protobuf schema (`kafka.proto`), auto-generates Rust bindings at build time via `tonic-build`, and provides two symmetric components: `KafkaBrokerServer` (Tonic server that accepts client connections and forwards requests to `BrokerCore`) and `KafkaBrokerClient` (Tonic client stub that producers and consumers use to talk to a broker).

The design decouples network I/O from business logic: handlers in `KafkaBrokerServer` contain no logic beyond serialization and channel forwarding. All logic lives in `BrokerCore`.

## 2. Responsibilities

- Define the `Broker` gRPC service and all message types in `kafka.proto` (11 RPCs)
- Auto-generate Rust client/server stubs via `build.rs` using `tonic-build`
- `KafkaBrokerServer`: implement the Tonic `Broker` service trait; for each RPC, create a `(oneshot_tx, oneshot_rx)` pair, send the request to `BrokerCore` via mpsc, and await the response on `oneshot_rx`
- `KafkaBrokerClient`: wrap the generated `BrokerClient<Channel>` in an `Arc<Mutex<...>>` for thread-safe shared use; expose a typed method for each of the 11 RPCs
- Define the `KafkaBrokerClientTrait` async trait to enable mock clients in tests
- Handle gRPC connection setup: endpoint URI, timeout, retry policy
- Propagate gRPC `Status` errors from the server back to the client

## 3. Non-Responsibilities

- Does not implement business logic (no offset resolution, no group coordination)
- Does not handle TLS/mTLS configuration (planned for the security feature)
- Does not load-balance across brokers (the client connects to a single endpoint)
- Does not perform automatic leader discovery or failover redirects (client must retry)

## 4. Architecture Design

```
Producer / Consumer (client side)
      |
      | calls KafkaBrokerClientTrait methods
      v
+--------------------------------------+
|         KafkaBrokerClient            |
|   Arc<Mutex<BrokerClient<Channel>>>  |
|   connect(addr) → Channel → stub     |
+--------------------------------------+
      |
      | gRPC over HTTP/2 (Protobuf)
      |
+--------------------------------------+
|       KafkaBrokerServer              |
|   impl broker_server::Broker         |
|   (Tonic service, per RPC handler)  |
+--------------------------------------+
      |
      | mpsc::send((BrokerGrpcRequest, oneshot_tx))
      v
BrokerCore<S>
```

### Build-time code generation

```
build.rs:
  tonic_build::compile_protos("src/api/kafka.proto")
  tonic_build::compile_protos("src/api/raft.proto")

Generated files (written to OUT_DIR, included via include!() macro):
  broker.rs  — BrokerClient<T>, broker_server::Broker trait, all message types
  raft.rs    — RaftClient<T>, raft_server::Raft trait, Raft message types
```

### Proto service (condensed)

```protobuf
service Broker {
  rpc GetTopicMetadata  (MetadataRequest)       returns (MetadataResponse);
  rpc Produce           (ProduceRequest)         returns (ProduceResponse);
  rpc Fetch             (FetchRequest)           returns (FetchResponse);
  rpc ListOffsets       (ListOffsetsRequest)     returns (ListOffsetsResponse);
  rpc FindCoordinator   (FindCoordinatorRequest) returns (FindCoordinatorResponse);
  rpc JoinGroup         (JoinGroupRequest)       returns (JoinGroupResponse);
  rpc SyncGroup         (SyncGroupRequest)       returns (SyncGroupResponse);
  rpc Heartbeat         (HeartbeatRequest)       returns (HeartbeatResponse);
  rpc LeaveGroup        (LeaveGroupRequest)      returns (LeaveGroupResponse);
  rpc CommitOffset      (OffsetCommitRequest)    returns (OffsetCommitResponse);
  rpc FetchOffset       (OffsetFetchRequest)     returns (OffsetFetchResponse);
}
```

## 5. Core Data Structures (Rust)

```rust
// src/broker/kafka_broker_server.rs

use tokio::sync::{mpsc, oneshot};

/// Tonic gRPC server implementing the Broker service.
/// Owns the mpsc sender used to forward requests to BrokerCore.
pub struct KafkaBrokerServer {
    /// Channel to send requests to BrokerCore.
    request_tx: mpsc::UnboundedSender<(BrokerGrpcRequest, oneshot::Sender<BrokerGrpcResponse>)>,
}

// src/client/kafka_broker_client.rs

/// Thread-safe gRPC client wrapper. Clone-able; all clones share the same channel.
#[derive(Clone)]
pub struct KafkaBrokerClient {
    inner: Arc<Mutex<BrokerClient<Channel>>>,
}

/// Trait enabling mock implementations for unit testing producers and consumers.
#[async_trait]
pub trait KafkaBrokerClientTrait: Send + Sync + Clone + 'static {
    async fn produce(&self, req: ProduceRequest)         -> Result<ProduceResponse, Status>;
    async fn fetch(&self, req: FetchRequest)             -> Result<FetchResponse, Status>;
    async fn list_offsets(&self, req: ListOffsetsRequest) -> Result<ListOffsetsResponse, Status>;
    async fn get_topic_metadata(&self, req: MetadataRequest) -> Result<MetadataResponse, Status>;
    async fn commit_offset(&self, req: OffsetCommitRequest) -> Result<OffsetCommitResponse, Status>;
    async fn fetch_offset(&self, req: OffsetFetchRequest)   -> Result<OffsetFetchResponse, Status>;
    async fn join_group(&self, req: JoinGroupRequest)   -> Result<JoinGroupResponse, Status>;
    async fn sync_group(&self, req: SyncGroupRequest)   -> Result<SyncGroupResponse, Status>;
    async fn heartbeat(&self, req: HeartbeatRequest)    -> Result<HeartbeatResponse, Status>;
    async fn leave_group(&self, req: LeaveGroupRequest) -> Result<LeaveGroupResponse, Status>;
    async fn find_coordinator(&self, req: FindCoordinatorRequest) -> Result<FindCoordinatorResponse, Status>;
}
```

## 6. Public Interfaces

```rust
// KafkaBrokerServer
impl KafkaBrokerServer {
    /// Create a server instance. Pair with BrokerCore::new() using the same channel.
    pub fn new(
        request_tx: mpsc::UnboundedSender<(BrokerGrpcRequest, oneshot::Sender<BrokerGrpcResponse>)>,
    ) -> Self;

    /// Start listening on `addr`. Returns when the server shuts down.
    /// Requires a Tokio runtime.
    pub async fn serve(self, addr: SocketAddr) -> anyhow::Result<()>;
}

// KafkaBrokerClient
impl KafkaBrokerClient {
    /// Connect to a broker at `addr` (e.g. "http://localhost:50051").
    pub async fn connect(addr: &str) -> anyhow::Result<Self>;
}

impl KafkaBrokerClientTrait for KafkaBrokerClient { ... }
```

## 7. Internal Algorithms

### Server RPC handler pattern (same for all 11 methods)

```
KafkaBrokerServer::produce(request: Request<ProduceRequest>)
  → Result<Response<ProduceResponse>, Status>:

  let req = request.into_inner()
  let (tx, rx) = oneshot::channel()
  self.request_tx
      .send((BrokerGrpcRequest::Produce(req), tx))
      .map_err(|_| Status::internal("broker core unavailable"))?

  let response = rx.await
      .map_err(|_| Status::internal("broker core dropped response channel"))?

  match response:
    BrokerGrpcResponse::Produce(r) → Ok(Response::new(r))
    _ → Err(Status::internal("unexpected response variant"))
```

### Client method pattern (same for all 11 methods)

```
KafkaBrokerClient::produce(req: ProduceRequest)
  → Result<ProduceResponse, Status>:

  let mut client = self.inner.lock().await
  let response = client.produce(Request::new(req)).await?
  Ok(response.into_inner())
```

### Server startup

```
KafkaBrokerServer::serve(addr):
  let service = broker_server::BrokerServer::new(self)
  Server::builder()
      .add_service(service)
      .serve(addr)
      .await
      .map_err(Into::into)
```

### KafkaBrokerClient connection with retry

```
KafkaBrokerClient::connect(addr):
  let endpoint = Channel::from_shared(addr.to_owned())?
      .timeout(Duration::from_secs(30))
      .connect_timeout(Duration::from_secs(5))
  let channel = endpoint.connect().await?
  let client = BrokerClient::new(channel)
  Ok(KafkaBrokerClient { inner: Arc::new(Mutex::new(client)) })
```

## 8. Persistence Model

The transport layer is stateless. No data is persisted by `KafkaBrokerServer` or `KafkaBrokerClient`. All persistence is handled by the storage backend behind `BrokerCore`.

## 9. Concurrency Model

| Object | Primitive | Usage |
|---|---|---|
| `KafkaBrokerServer.request_tx` | `mpsc::UnboundedSender` | `Clone`d into each Tonic handler task by Tonic's `Clone`-based dispatch |
| `KafkaBrokerClient.inner` | `Arc<Mutex<BrokerClient<Channel>>>` | Shared across `Producer` and `Consumer`; lock held only during the gRPC call |
| Tonic server tasks | Tonic spawns one task per active RPC | Handlers are `Send + 'static`; all state accessed via `Clone`d sender |

**Per-request oneshot**: Each in-flight RPC has its own `oneshot::channel`. There is no shared response state. Multiple RPCs can be in-flight simultaneously; each independently awaits its own response.

## 10. Configuration

```rust
pub struct TransportConfig {
    /// Address the broker server listens on (e.g. "0.0.0.0:50051").
    pub listen_addr: SocketAddr,
    /// Request/response timeout for client-side calls (seconds).
    pub timeout_secs: u64,
    /// Number of times the client retries a failed request.
    pub max_retries: u32,
}
```

Defaults: `listen_addr = 0.0.0.0:50051`, `timeout_secs = 30`, `max_retries = 3`.

## 11. Observability

- `KafkaBrokerServer`: every RPC logged at `DEBUG` with method name and remote address
- `KafkaBrokerServer`: every error response logged at `WARN` with status code and message
- `KafkaBrokerClient`: connection establishment logged at `INFO`; connection failure at `ERROR`
- `KafkaBrokerClient`: each retry attempt logged at `WARN` with attempt number and error

## 12. Testing Strategy

**Unit tests** (`KafkaBrokerServer` + `BrokerCore` + `InMemoryStorage`, no real network):
- `test_produce_roundtrip`: send Produce through server → core → storage, assert response offsets
- `test_fetch_after_produce`: produce then fetch, assert message values match
- `test_server_returns_error_on_core_unavailable`: drop all mpsc receivers, assert server returns `INTERNAL` status

**Integration tests** (real Tonic, loopback network, `InMemoryStorage`):
- `test_client_connect_and_produce`: `KafkaBrokerClient::connect` to an in-process server, send ProduceRequest, assert non-error response
- `test_client_fetch_produced_messages`: produce 10 messages, fetch them all, assert offsets 0–9
- `test_concurrent_clients`: 5 concurrent `KafkaBrokerClient` instances producing to the same partition, assert no duplicate offsets
- `test_client_timeout`: server blocked; assert client returns `Status::DeadlineExceeded` within `timeout_secs`

**Proto schema tests**:
- `test_all_request_variants_round_trip`: serialize and deserialize each request type through prost, assert field equality

## 13. Open Questions

None.
