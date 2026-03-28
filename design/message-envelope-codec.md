## Message Envelope Codec

### Purpose

Provide a versioned application-level payload envelope and encode/decode helpers for binary and JSON message formats.

### Scope

**In scope:**
- `MessageEnvelope<T>` and codec helpers in [`src/codec.rs`](../src/codec.rs).
- Error type `MessageCodecError`.

**Out of scope:**
- Broker protocol wire format (`kafka.proto`, `raft.proto`).
- Producer partition routing and consumer loop behavior.

### Primary User Flow

1. Application wraps typed payload in `MessageEnvelope::new(event_type, schema_version, payload)`.
2. Producer encodes envelope bytes with `encode` (bincode) or `encode_json`.
3. Consumer decodes bytes with `decode` or `decode_json` into typed payload.

### System Flow

1. Envelope constructor captures `created_at_ms` from current system time.
2. Binary encode/decode uses `bincode::{serialize, deserialize}`.
3. JSON encode/decode uses `serde_json::{to_vec, from_slice}`.
4. Errors are mapped into `MessageCodecError` variants for caller handling.

### Data Model

- `MessageEnvelope<T>` fields:
- `event_type: String`
- `schema_version: u16`
- `created_at_ms: i64`
- `payload: T`
- `MessageCodecError` variants:
- `EncodeBinary`, `DecodeBinary`, `EncodeJson`, `DecodeJson`.

Persistence behavior:
- Encoded bytes are transient payload representation; persistence depends on caller/broker usage.

### Interfaces and Contracts

- Public APIs:
- `MessageEnvelope::new`
- `encode`, `decode`
- `encode_json`, `decode_json`
- Contract: payload type must satisfy serde traits for chosen format.

### Dependencies

**Internal modules:**
- Used by examples to serialize app payloads before producer send.

**External services/libraries:**
- `serde`, `serde_json`, `bincode`.

### Failure Modes and Edge Cases

- Invalid bytes for chosen format return decode errors.
- Generic type mismatch at decode site returns deserialization error.
- `created_at_ms` uses system clock and is not monotonic.

### Observability and Debugging

- No built-in logs in codec module.
- Debug by inspecting `MessageCodecError` variant and message.

### Risks and Notes

- Schema compatibility across versions is caller-managed; module only stores `schema_version` without migration support.
- Binary format couples producer and consumer to bincode representation details.

Changes:

