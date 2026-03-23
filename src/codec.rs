use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// A small, versioned envelope for application-level message contracts.
///
/// The MQ broker stores raw bytes only. This envelope provides a simple
/// convention for encoding typed payloads into bytes and decoding them back.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageEnvelope<T> {
    /// Logical event/type name (e.g. "order.created").
    pub event_type: String,
    /// Application schema version.
    pub schema_version: u16,
    /// Creation timestamp in unix milliseconds.
    pub created_at_ms: i64,
    /// Typed payload.
    pub payload: T,
}

impl<T> MessageEnvelope<T> {
    pub fn new(event_type: impl Into<String>, schema_version: u16, payload: T) -> Self {
        Self {
            event_type: event_type.into(),
            schema_version,
            created_at_ms: now_ms(),
            payload,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MessageCodecError {
    #[error("failed to encode envelope to binary: {0}")]
    EncodeBinary(bincode::Error),
    #[error("failed to decode envelope from binary: {0}")]
    DecodeBinary(bincode::Error),
    #[error("failed to encode envelope to JSON: {0}")]
    EncodeJson(serde_json::Error),
    #[error("failed to decode envelope from JSON: {0}")]
    DecodeJson(serde_json::Error),
}

/// Encode a typed envelope as compact binary bytes (bincode).
///
/// This is the recommended fast path for Rust-to-Rust clients.
pub fn encode<T: Serialize>(envelope: &MessageEnvelope<T>) -> Result<Vec<u8>, MessageCodecError> {
    bincode::serialize(envelope).map_err(MessageCodecError::EncodeBinary)
}

/// Decode compact binary bytes (bincode) into a typed envelope.
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<MessageEnvelope<T>, MessageCodecError> {
    bincode::deserialize(bytes).map_err(MessageCodecError::DecodeBinary)
}

/// Encode a typed envelope as JSON bytes.
///
/// Keep this for interoperability with non-Rust consumers.
pub fn encode_json<T: Serialize>(
    envelope: &MessageEnvelope<T>,
) -> Result<Vec<u8>, MessageCodecError> {
    serde_json::to_vec(envelope).map_err(MessageCodecError::EncodeJson)
}

/// Decode JSON bytes into a typed envelope.
pub fn decode_json<T: DeserializeOwned>(
    bytes: &[u8],
) -> Result<MessageEnvelope<T>, MessageCodecError> {
    serde_json::from_slice(bytes).map_err(MessageCodecError::DecodeJson)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    struct OrderCreated {
        order_id: String,
        amount_cents: u64,
    }

    #[test]
    fn binary_codec_round_trip() {
        let envelope = MessageEnvelope::new(
            "order.created",
            1,
            OrderCreated {
                order_id: "ORD-1001".to_string(),
                amount_cents: 1250,
            },
        );

        let encoded = encode(&envelope).expect("encode should succeed");
        let decoded: MessageEnvelope<OrderCreated> =
            decode(&encoded).expect("decode should succeed");

        assert_eq!(decoded.event_type, "order.created");
        assert_eq!(decoded.schema_version, 1);
        assert_eq!(decoded.payload.order_id, "ORD-1001");
        assert_eq!(decoded.payload.amount_cents, 1250);
        assert!(decoded.created_at_ms > 0);
    }

    #[test]
    fn binary_decode_invalid_bytes() {
        let bad = b"not-bincode";
        let result: Result<MessageEnvelope<OrderCreated>, MessageCodecError> = decode(bad);
        assert!(matches!(result, Err(MessageCodecError::DecodeBinary(_))));
    }

    #[test]
    fn binary_is_smaller_than_json_for_typical_payload() {
        let envelope = MessageEnvelope::new(
            "order.created",
            1,
            OrderCreated {
                order_id: "ORD-1001".to_string(),
                amount_cents: 1250,
            },
        );
        let binary = encode(&envelope).expect("binary encode should succeed");
        let json = encode_json(&envelope).expect("json encode should succeed");
        assert!(binary.len() < json.len());
    }
}
