use openraft::{Config, NodeId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt::Display;
use std::io::Cursor;

/// Node ID type for Raft cluster
pub type BrokerNodeId = u64;

/// Node information
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, Default)]
pub struct BrokerNode {
    pub rpc_addr: String,
    pub api_addr: String,
}

impl Display for BrokerNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "BrokerNode {{ rpc: {}, api: {} }}", self.rpc_addr, self.api_addr)
    }
}

/// Type configuration for OpenRaft
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Ord, PartialOrd)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TypeConfig {}

impl openraft::RaftTypeConfig for TypeConfig {
    type D = BrokerRequest;
    type R = BrokerResponse;
    type NodeId = BrokerNodeId;
    type Node = BrokerNode;
    type Entry = openraft::Entry<TypeConfig>;
    type SnapshotData = Cursor<Vec<u8>>;
    type AsyncRuntime = openraft::TokioRuntime;
    type Responder = openraft::impls::OneshotResponder<TypeConfig>;
}

/// Broker request that will be replicated via Raft
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BrokerRequest {
    /// Produce message to topic
    Produce {
        topic: String,
        partition: i32,
        message: Vec<u8>,
    },
    /// Commit consumer offset
    CommitOffset {
        group: String,
        topic: String,
        partition: i32,
        offset: i64,
        metadata: String,
    },
    /// Join consumer group
    JoinGroup {
        group_id: String,
        member_id: String,
        protocol_type: String,
    },
    /// Leave consumer group
    LeaveGroup {
        group_id: String,
        member_id: String,
    },
    /// Create topic
    CreateTopic {
        topic: String,
        partitions: i32,
        replication_factor: i32,
    },
    /// Delete topic
    DeleteTopic {
        topic: String,
    },
    /// Heartbeat for group membership
    GroupHeartbeat {
        group_id: String,
        generation_id: i32,
        member_id: String,
    },
}

/// Response for broker operations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BrokerResponse {
    Produce {
        offset: i64,
    },
    CommitOffset,
    JoinGroup {
        generation_id: i32,
        leader_id: String,
        member_id: String,
    },
    LeaveGroup,
    CreateTopic,
    DeleteTopic,
    GroupHeartbeat,
    Error(String),
}

/// Application data that will be stored in Raft state machine
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrokerStateMachineData {
    /// Topic -> Partition -> Messages
    pub messages: BTreeMap<String, BTreeMap<i32, Vec<(i64, Vec<u8>)>>>,
    
    /// Group -> Topic -> Partition -> (Offset, Metadata)
    pub offsets: BTreeMap<String, BTreeMap<String, BTreeMap<i32, (i64, String)>>>,
    
    /// Consumer groups
    pub groups: BTreeMap<String, ConsumerGroupState>,
    
    /// Topic configurations
    pub topics: BTreeMap<String, TopicConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsumerGroupState {
    pub generation_id: i32,
    pub leader_id: String,
    pub protocol: String,
    pub members: Vec<GroupMember>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupMember {
    pub member_id: String,
    pub metadata: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicConfig {
    pub partitions: i32,
    pub replication_factor: i32,
}

impl Default for BrokerStateMachineData {
    fn default() -> Self {
        Self {
            messages: BTreeMap::new(),
            offsets: BTreeMap::new(),
            groups: BTreeMap::new(),
            topics: BTreeMap::new(),
        }
    }
}

/// Create default Raft configuration
pub fn create_raft_config() -> Config {
    Config {
        heartbeat_interval: 1000,
        election_timeout_min: 3000,
        election_timeout_max: 6000,
        ..Default::default()
    }
}
