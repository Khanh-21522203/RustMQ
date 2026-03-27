use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

// ── Replicated state machine ──────────────────────────────────────────────────

/// The replicated state machine applied by Raft committed entries.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct BrokerData {
    /// (topic, partition) → ordered message log; index == offset
    pub messages: HashMap<(String, i32), Vec<BrokerStoredMessage>>,
    /// (group_id, topic, partition) → committed consumer offset
    pub offsets: HashMap<(String, String, i32), i64>,
    /// topic → num_partitions
    pub topics: HashMap<String, i32>,
    /// group_id → replicated group state
    pub groups: HashMap<String, ReplicatedGroupState>,
    /// (topic, partition) → next offset watermark; monotonically increasing, survives truncation
    #[serde(default)]
    pub next_offsets: HashMap<(String, i32), i64>,
}

/// Serializable stored message for Raft log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrokerStoredMessage {
    pub offset: i64,
    pub key: Option<Vec<u8>>,
    pub value: Vec<u8>,
    pub timestamp_ms: i64,
}

// ── Consumer group state ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum GroupStatus {
    #[default]
    Empty,
    Stable,
    PreparingRebalance,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplicatedGroupMember {
    pub member_id: String,
    pub metadata: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReplicatedGroupState {
    pub generation_id: i32,
    pub leader_id: String,
    pub members: Vec<ReplicatedGroupMember>,
    pub subscriptions: HashMap<String, String>, // member_id → topic
    pub assignments: HashMap<String, Vec<i32>>, // member_id → partitions
    pub status: GroupStatus,
    pub rejoined: HashSet<String>,
    pub session_timeout_ms: i64,
    pub rebalance_started_ms: i64,
}

// ── Commands serialized into Raft log ─────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BrokerCommand {
    Produce {
        topic: String,
        partition: i32,
        key: Option<Vec<u8>>,
        value: Vec<u8>,
    },
    CommitOffset {
        group_id: String,
        topic: String,
        partition: i32,
        offset: i64,
    },
    CreateTopic {
        topic: String,
        num_partitions: i32,
    },
    TruncatePartition {
        topic: String,
        partition: i32,
        before_offset: i64,
    },
    GroupJoin {
        group_id: String,
        member_id: String,
        topic: String,
        metadata: Vec<u8>,
        session_timeout_ms: i64,
    },
    GroupLeave {
        group_id: String,
        member_id: String,
    },
    GroupExpire {
        group_id: String,
        expired_ids: Vec<String>,
    },
    GroupFinalize {
        group_id: String,
    },
}

// ── Sled persistence ──────────────────────────────────────────────────────────

/// Reconstruct BrokerData by scanning all sled keys.
pub fn load_broker_data_from_sled(db: &sled::Db) -> BrokerData {
    let mut data = BrokerData::default();

    for item in db.scan_prefix(b"msg:") {
        if let Ok((key, val)) = item {
            if let (Ok(key_str), Ok(msg)) = (
                std::str::from_utf8(&key),
                bincode::deserialize::<BrokerStoredMessage>(&val),
            ) {
                // key: "msg:{topic}:{partition:010}:{offset:020}"
                let parts: Vec<&str> = key_str.splitn(4, ':').collect();
                if parts.len() == 4 {
                    if let Ok(partition) = parts[2].parse::<i32>() {
                        data.messages
                            .entry((parts[1].to_string(), partition))
                            .or_default()
                            .push(msg);
                    }
                }
            }
        }
    }
    for log in data.messages.values_mut() {
        log.sort_by_key(|m| m.offset);
    }

    for item in db.scan_prefix(b"off:") {
        if let Ok((key, val)) = item {
            if let (Ok(key_str), Ok(offset)) =
                (std::str::from_utf8(&key), bincode::deserialize::<i64>(&val))
            {
                // key: "off:{group}:{topic}:{partition:010}"
                let parts: Vec<&str> = key_str.splitn(4, ':').collect();
                if parts.len() == 4 {
                    if let Ok(partition) = parts[3].parse::<i32>() {
                        data.offsets.insert(
                            (parts[1].to_string(), parts[2].to_string(), partition),
                            offset,
                        );
                    }
                }
            }
        }
    }

    for item in db.scan_prefix(b"top:") {
        if let Ok((key, val)) = item {
            if let (Ok(key_str), Ok(num_parts)) =
                (std::str::from_utf8(&key), bincode::deserialize::<i32>(&val))
            {
                // key: "top:{topic}"
                if let Some(topic) = key_str.strip_prefix("top:") {
                    data.topics.insert(topic.to_string(), num_parts);
                }
            }
        }
    }

    for item in db.scan_prefix(b"grp:") {
        if let Ok((key, val)) = item {
            if let (Ok(key_str), Ok(group)) = (
                std::str::from_utf8(&key),
                bincode::deserialize::<ReplicatedGroupState>(&val),
            ) {
                // key: "grp:{group_id}"
                if let Some(group_id) = key_str.strip_prefix("grp:") {
                    data.groups.insert(group_id.to_string(), group);
                }
            }
        }
    }

    // Load high-watermark offsets; key: "hwm:{topic}:{partition:010}"
    for item in db.scan_prefix(b"hwm:") {
        if let Ok((key, val)) = item {
            if let (Ok(key_str), Ok(next)) =
                (std::str::from_utf8(&key), bincode::deserialize::<i64>(&val))
            {
                let parts: Vec<&str> = key_str.splitn(3, ':').collect();
                if parts.len() == 3 {
                    if let Ok(partition) = parts[2].parse::<i32>() {
                        let topic = parts[1].to_string();
                        let entry = data.next_offsets.entry((topic, partition)).or_insert(0);
                        *entry = (*entry).max(next);
                    }
                }
            }
        }
    }

    data
}
