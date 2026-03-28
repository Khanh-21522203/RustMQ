## Broker Topic and Data RPC Path

### Purpose

Handle topic metadata, topic creation, produce, fetch, and list-offset operations against the active broker storage backend (`InMemoryStorage` or `KRaftBroker`).

### Scope

**In scope:**
- `handle_get_topic_metadata`, `handle_create_topic`, `handle_produce`, `handle_fetch`, `handle_list_offsets` in [`src/broker/server/core.rs`](../src/broker/server/core.rs).
- Corresponding RPC messages in [`src/api/kafka.proto`](../src/api/kafka.proto).
- Storage trait contracts in [`src/broker/storage/traits.rs`](../src/broker/storage/traits.rs).

**Out of scope:**
- Consumer group membership protocol and offset commit/fetch.
- Raft metadata command pipeline internals.

### Primary User Flow

1. Producer/consumer/admin client sends topic/data RPCs.
2. Broker validates/request-routes and calls storage trait methods.
3. Broker maps storage results into protocol responses and error codes.
4. Client receives offsets, records, or metadata.

### System Flow

1. `GetTopicMetadata`:
- Resolve topic list from request or `storage.get_topics()`.
- Query partition ids and cluster metadata.
- Build `TopicMetadataResponse` with broker list and partition metadata.
2. `CreateTopic`:
- Call `storage.create_topic(topic_name, num_partitions)`.
3. `Produce`:
- For each record, call `produce_message` or `produce_message_acks_all` (`required_acks == -1`).
- Map `BrokerError::NotLeader` to `error_code = 6` and `leader_addr`.
4. `Fetch`:
- Call `storage.fetch_messages`, convert `StoredMessage` to `FetchedRecord`.
- Compute response `high_watermark_offset` from batch tail (`last.offset + 1`) when records exist.
5. `ListOffsets`:
- Call `storage.get_partition_offset(topic, partition, time)`.

### Data Model

- Produce request structures:
- `ProduceRequest { required_acks, timeout_ms, topics[] }`
- `Record { key?: bytes, value: bytes }`
- Fetch request structures:
- `FetchRequest { replica_id, max_wait_time, min_bytes, topics[] }`
- Response structures:
- `ProduceResponse.PartitionResult { partition, error_code, offset, leader_addr }`
- `FetchResponse.PartitionResult { partition, error_code, high_watermark_offset, records[] }`
- `ListOffsetsResponse.PartitionOffsets { partition, error_code, offsets[] }`
- Storage entity:
- `StoredMessage { offset: i64, key: Option<Vec<u8>>, value: Vec<u8>, timestamp_ms: i64 }`.

Persistence behavior:
- In-memory backend stores messages/offsets in process memory maps.
- KRaft backend stores partition data in sled-backed `PartitionLog` trees.

### Interfaces and Contracts

- RPCs: `GetTopicMetadata`, `CreateTopic`, `Produce`, `Fetch`, `ListOffsets`.
- Error code mapping used by core:
- `0` = success.
- `6` = not leader for partition (with redirect address when available).
- `1` = generic broker/storage failure.

### Dependencies

**Internal modules:**
- [`src/broker/storage/traits.rs`](../src/broker/storage/traits.rs) for storage trait and in-memory impl.
- [`src/broker/kraft/broker.rs`](../src/broker/kraft/broker.rs) for KRaft-backed storage behavior.

**External services/libraries:**
- No direct external service at this layer; depends on selected storage backend.

### Failure Modes and Edge Cases

- Unknown topics/partitions may return empty collections instead of explicit errors in some paths.
- `required_acks` values other than `-1` use non-acks-all path.
- `FetchRequest.max_wait_time`, `min_bytes`, and `ListOffsets.max_number_of_offsets` are not enforced in core logic.
- `GetTopicMetadata` currently uses cluster leader id for all partition leaders in response construction.

### Observability and Debugging

- Router logs operation summaries (topic names, partition lists, record counts).
- Core logs errors on produce/fetch/list-offset failures.
- Debug produce routing issues by checking `NotLeader` mapping and returned `leader_addr`.

### Risks and Notes

- Response `high_watermark_offset` in `handle_fetch` is derived from returned records, not authoritative storage HW.
- Mixed backend semantics (in-memory vs KRaft) can expose different operational behavior behind same RPC contract.

Changes:

