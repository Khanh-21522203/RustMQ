## Metadata Cache and RPC Client

### Purpose

Provide low-level broker RPC wrappers and a metadata cache that maps `(topic, partition)` to leader broker addresses.

### Scope

**In scope:**
- `KafkaBrokerClient` and `KafkaBrokerClientTrait` in [`src/client/kafka_broker_client.rs`](../src/client/kafka_broker_client.rs).
- `MetadataCache` in [`src/client/metadata_cache.rs`](../src/client/metadata_cache.rs).
- Endpoint formatting helper in [`src/utils.rs`](../src/utils.rs).

**Out of scope:**
- Higher-level producer/consumer application loops.

### Primary User Flow

1. Client constructs `KafkaBrokerClient` with broker endpoint.
2. Callers invoke typed RPC wrappers.
3. Producer path uses `MetadataCache` to refresh metadata and reuse per-broker clients.
4. Leader map is invalidated and refreshed on routing failures.

### System Flow

1. `KafkaBrokerClient::new(addr)` normalizes endpoint and opens tonic channel.
2. Each trait method locks underlying tonic client and executes one broker RPC.
3. `MetadataCache::refresh(topic)`:
- Query bootstrap broker `GetTopicMetadata`.
- Build `node_id -> host:port` map from response brokers.
- Store `leaders[(topic, partition)] = broker_addr` for partitions with no partition error.
4. `resolve_leaders(topic, partitions)` ensures cache warm-up and returns grouped `broker_addr -> partitions`.

### Data Model

- `KafkaBrokerClient { client: Arc<Mutex<BrokerClient<Channel>>> }`.
- `MetadataCache` fields:
- `bootstrap_addr`,
- `leaders: HashMap<(String, i32), String>`,
- `connections: HashMap<String, Arc<KafkaBrokerClient>>`.

Persistence behavior:
- Client/channel and metadata maps are process-local in-memory caches.

### Interfaces and Contracts

- `KafkaBrokerClientTrait` methods wrap all broker RPC methods used by client runtime.
- Address normalization contract:
- `format_endpoint_addr` prepends `http://` when missing.
- `MetadataCache` canonicalizes stored broker addresses to `host:port` (no scheme).

### Dependencies

**Internal modules:**
- Generated broker gRPC client from `src/api/broker.rs`.
- Producer/consumer call sites.

**External services/libraries:**
- `tonic` channel/client.
- `tokio::sync::Mutex` for serialized client access.

### Failure Modes and Edge Cases

- Bootstrap broker unavailability prevents metadata refresh.
- Metadata response missing leader node mapping causes warning and unresolved partition.
- No cache eviction strategy; stale/unused connections may accumulate.

### Observability and Debugging

- Debug logs on leader cache inserts and partition metadata errors.
- Warn logs when leader node id is absent from broker list or connection fails.
- First debug point for routing errors is `MetadataCache::resolve_leaders`.

### Risks and Notes

- Single mutex per `KafkaBrokerClient` serializes RPC calls on that client instance.
- Metadata correctness depends on broker-side `TopicMetadataResponse` accuracy.

Changes:

