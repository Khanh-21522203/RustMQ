## Producer Leader Routing and Batching

### Purpose

Provide producer APIs that batch records, assign partitions, resolve partition leaders, and retry on `NOT_LEADER` or transport failures.

### Scope

**In scope:**
- Producer runtime in [`src/client/producer.rs`](../src/client/producer.rs).
- Metadata cache interaction in [`src/client/metadata_cache.rs`](../src/client/metadata_cache.rs).

**Out of scope:**
- Broker-side produce handling.
- Consumer runtime behavior.

### Primary User Flow

1. Caller builds `ProducerConfig` and creates `Producer::new`.
2. Caller sends records with `send` (batched) or `send_sync` (await response).
3. Producer resolves leader per partition and routes `ProduceRequest` directly.
4. Caller flushes/shuts down producer.

### System Flow

1. `Producer::new` initializes metadata cache, batch buffer, and background flush task.
2. `send` appends message to in-memory batch and triggers immediate send if `batch_size` reached.
3. `send_batch_inner`:
- Assign partition (`fixed`, `round_robin`, `key_hash`, or explicit message partition).
- Resolve leaders for all partitions.
- Group partition payloads by broker address.
- Send one `ProduceRequest` per broker.
- On transport error or `error_code == 6`, invalidate topic metadata and retry (max 3 attempts, increasing sleep).
4. `send_sync` follows same leader-resolution retry strategy for single message and returns `ProducerResult` with partition/offset.

### Data Model

- `ProducerMessage { key: Option<Vec<u8>>, value: Vec<u8>, partition: Option<i32> }`.
- `ProducerResult { partition: i32, offset: i64, error_code: i32 }`.
- `Producer` internal state:
- `config`,
- `meta: Arc<Mutex<MetadataCache>>`,
- `batch: Arc<Mutex<Vec<ProducerMessage>>>`,
- `shutdown_tx`,
- `round_robin_counter`.

Persistence behavior:
- Batched records are in-memory until sent.
- Producer itself is stateless across process restart.

### Interfaces and Contracts

- Public methods:
- `Producer::new`, `send`, `send_sync`, `flush`, `shutdown`.
- `ProducerMessage` builders:
- `new`, `with_key`, `to_partition`.
- Error-code contract used for leader refresh:
- `6` treated as `NOT_LEADER_FOR_PARTITION`.

### Dependencies

**Internal modules:**
- `MetadataCache` for leader and client reuse.
- `KafkaBrokerClientTrait::produce` RPC wrapper.

**External services/libraries:**
- `tokio` for flush loop, retries, and channels.
- `tonic::Request` for RPC wrapper inputs.

### Failure Modes and Edge Cases

- Batch send hard-fails when partition returns non-zero, non-6 error code.
- `key_hash` strategy without key falls back to configured fixed partition.
- If metadata refresh still cannot resolve leader, send fails.
- Background flush task errors are logged; caller only sees errors for explicit `send`/`flush`/`send_sync` calls.

### Observability and Debugging

- Logs include retry attempt numbers, transport errors, `NOT_LEADER` signals, and successful partition offsets.
- Debug partition routing by inspecting `assign_partition` and metadata cache entries.

### Risks and Notes

- Background flush task is detached and not joined; shutdown correctness depends on signaling plus explicit flush.
- `send` success means message accepted into batch/send path, not necessarily end-to-end consumer processing.
- No explicit memory cap for queued batch beyond caller-driven send/flush cadence.

Changes:

