use serde::{Deserialize, Serialize};
use std::fs;

/// Broker configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrokerConfig {
    /// Node ID for this broker
    pub node_id: u64,
    
    /// API address for client connections
    pub api_addr: String,
    
    /// RPC address for Raft inter-node communication
    pub rpc_addr: String,
    
    /// Storage path for Raft data
    pub storage_path: String,
    
    /// Cluster configuration
    pub cluster: ClusterConfig,
    
    /// Raft configuration
    pub raft: RaftConfig,

    /// Message retention
    #[serde(default)]
    pub retention: RetentionConfig,

    /// Log level
    pub log_level: String,
}

/// Cluster configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterConfig {
    /// Initial cluster members
    pub initial_members: Vec<ClusterMember>,
    
    /// Whether to bootstrap a new cluster
    pub bootstrap: bool,
}

/// Cluster member information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterMember {
    pub node_id: u64,
    pub api_addr: String,
    pub rpc_addr: String,
}

/// Message retention configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RetentionConfig {
    /// Drop messages older than this many milliseconds (None = keep forever)
    pub retention_ms: Option<u64>,
    /// Maximum messages to keep per partition (None = unlimited)
    pub max_messages_per_partition: Option<usize>,
}

/// Raft configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RaftConfig {
    /// Heartbeat interval in milliseconds
    pub heartbeat_interval_ms: u64,
    
    /// Minimum election timeout in milliseconds
    pub election_timeout_min_ms: u64,
    
    /// Maximum election timeout in milliseconds
    pub election_timeout_max_ms: u64,
    
    /// Snapshot threshold (number of logs before snapshot)
    pub snapshot_threshold: u64,
}

impl BrokerConfig {
    /// Load configuration from a YAML file
    pub fn from_file(path: &str) -> anyhow::Result<Self> {
        let content = fs::read_to_string(path)?;
        let config = serde_yaml::from_str(&content)?;
        Ok(config)
    }
    
    /// Create a default single-node configuration
    pub fn default_single_node() -> Self {
        Self {
            node_id: 1,
            api_addr: "127.0.0.1:9092".to_string(),
            rpc_addr: "127.0.0.1:19092".to_string(),
            storage_path: "./data/broker-1".to_string(),
            cluster: ClusterConfig {
                initial_members: vec![
                    ClusterMember {
                        node_id: 1,
                        api_addr: "127.0.0.1:9092".to_string(),
                        rpc_addr: "127.0.0.1:19092".to_string(),
                    }
                ],
                bootstrap: true,
            },
            raft: RaftConfig {
                heartbeat_interval_ms: 1000,
                election_timeout_min_ms: 3000,
                election_timeout_max_ms: 6000,
                snapshot_threshold: 10000,
            },
            retention: RetentionConfig::default(),
            log_level: "info".to_string(),
        }
    }
}

// Removed openraft conversion as we're using raft-rs now
