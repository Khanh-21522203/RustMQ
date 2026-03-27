use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Cluster metadata maintained by the controller quorum (Raft state machine).
/// Only metadata lives here — message data lives in per-broker `PartitionLog`s.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ControllerMetadata {
    /// topic → TopicRecord
    pub topics: HashMap<String, TopicRecord>,
    /// (topic, partition) → PartitionRecord
    pub partitions: HashMap<(String, i32), PartitionRecord>,
    /// broker_id → BrokerRegistration
    pub brokers: HashMap<u64, BrokerRegistration>,
    /// Monotonically increasing epoch bumped on every controller election.
    pub controller_epoch: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicRecord {
    pub num_partitions: i32,
    pub replication_factor: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartitionRecord {
    /// Node ID of the current partition leader (0 = unassigned).
    pub leader: u64,
    /// In-sync replica set: fully caught-up followers (includes leader).
    pub isr: Vec<u64>,
    /// All assigned replicas (superset of ISR).
    pub replicas: Vec<u64>,
    /// Bumped every time leadership changes; used to fence stale leaders.
    pub leader_epoch: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrokerRegistration {
    pub broker_id: u64,
    /// Client-facing address e.g. "127.0.0.1:9092"
    pub api_addr: String,
    /// Inter-broker RPC address e.g. "127.0.0.1:9093"
    pub rpc_addr: String,
    /// Wall-clock ms of the last heartbeat seen from this broker (0 = never seen).
    #[serde(default)]
    pub last_seen_ms: u64,
}

/// Carries a single partition reassignment computed by the failure detector.
/// Passed inside `ControllerCommand::MarkBrokerDead` so the state machine only
/// applies data and makes no topology decisions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartitionAssignment {
    pub topic: String,
    pub partition: i32,
    /// New leader node ID; 0 means the partition is OFFLINE (ISR exhausted).
    pub new_leader: u64,
    pub new_isr: Vec<u64>,
    pub replicas: Vec<u64>,
}

/// Commands written into the controller Raft log.
/// Only cluster metadata changes belong here — no message data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ControllerCommand {
    CreateTopic {
        topic: String,
        num_partitions: i32,
        replication_factor: u16,
    },
    DeleteTopic {
        topic: String,
    },
    /// Update partition leadership and/or ISR membership.
    PartitionChange {
        topic: String,
        partition: i32,
        leader: u64,
        isr: Vec<u64>,
        replicas: Vec<u64>,
    },
    RegisterBroker {
        broker_id: u64,
        api_addr: String,
        rpc_addr: String,
    },
    UnregisterBroker {
        broker_id: u64,
    },
    BumpControllerEpoch,
    /// Broker-side heartbeat: update `last_seen_ms` in the broker registry.
    /// `timestamp_ms` is captured at proposal time (not inside apply) for
    /// Raft determinism.
    BrokerHeartbeat {
        broker_id: u64,
        timestamp_ms: u64,
    },
    /// Declare a broker dead and atomically apply all resulting partition
    /// reassignments.  The pre-computed assignments are carried in the command
    /// so the state machine stays pure (no topology logic inside apply).
    MarkBrokerDead {
        broker_id: u64,
        new_assignments: Vec<PartitionAssignment>,
    },
}
