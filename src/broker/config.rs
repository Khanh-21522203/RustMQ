use serde::{Deserialize, Serialize};
use std::fs;

/// Broker configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrokerConfig {
    /// Node ID for this broker
    #[serde(default = "default_node_id")]
    pub node_id: u64,

    /// API address for client connections
    #[serde(default = "default_api_addr")]
    pub api_addr: String,

    /// RPC address for Raft inter-node communication
    #[serde(default = "default_rpc_addr")]
    pub rpc_addr: String,

    /// Storage path for Raft data
    #[serde(default = "default_storage_path")]
    pub storage_path: String,

    /// Cluster configuration
    #[serde(default)]
    pub cluster: Option<ClusterConfig>,

    /// Raft configuration
    #[serde(default)]
    pub raft: Option<RaftConfig>,

    /// Message retention
    #[serde(default)]
    pub retention: RetentionConfig,

    /// Log level
    #[serde(default = "default_log_level")]
    pub log_level: String,

    /// Inter-broker Raft transport: "grpc" (default) or "sbe_tcp"
    #[serde(default = "default_transport")]
    pub transport: String,

    /// Address of an existing cluster node to join (enables dynamic membership).
    /// When set, this node sends an AddNode RPC to that address after startup.
    #[serde(default)]
    pub join_addr: Option<String>,

    /// Default replication factor for newly created topics.
    /// Must be <= the number of live brokers; clamped automatically otherwise.
    #[serde(default = "default_replication_factor")]
    pub default_replication_factor: u16,

    /// Optional shared token required for AddNode/RemoveNode RPCs.
    /// When set, clients must provide `x-rustmq-admin-token` metadata.
    #[serde(default)]
    pub membership_api_token: Option<String>,
}

/// Cluster configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterConfig {
    /// Initial cluster members
    pub initial_members: Vec<ClusterMember>,

    /// Whether to bootstrap a new cluster
    #[serde(default)]
    pub bootstrap: bool,
}

/// Cluster member information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterMember {
    pub node_id: u64,
    pub api_addr: String,
    pub rpc_addr: String,
    /// TCP address for SBE transport (only used when transport = "sbe_tcp")
    /// Defaults to rpc_addr if absent.
    #[serde(default)]
    pub sbe_tcp_addr: Option<String>,
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
    #[serde(default = "default_raft_heartbeat_interval_ms")]
    pub heartbeat_interval_ms: u64,

    /// Minimum election timeout in milliseconds
    #[serde(default = "default_election_timeout_min_ms")]
    pub election_timeout_min_ms: u64,

    /// Maximum election timeout in milliseconds
    #[serde(default = "default_election_timeout_max_ms")]
    pub election_timeout_max_ms: u64,

    /// Snapshot threshold (number of logs before snapshot)
    #[serde(default = "default_snapshot_threshold")]
    pub snapshot_threshold: u64,

    /// How long to wait for all members to rejoin before forcing rebalance finalization (ms)
    #[serde(default = "default_rebalance_timeout_ms")]
    pub rebalance_timeout_ms: i64,

    /// Interval for broker heartbeat proposals to controller metadata (ms).
    #[serde(default = "default_broker_heartbeat_propose_interval_ms")]
    pub broker_heartbeat_propose_interval_ms: u64,

    /// How often the controller scans for dead brokers (ms).
    #[serde(default = "default_failure_detector_interval_ms")]
    pub failure_detector_interval_ms: u64,

    /// A broker is considered missed when `now - last_seen_ms` exceeds this threshold (ms).
    #[serde(default = "default_dead_broker_threshold_ms")]
    pub dead_broker_threshold_ms: u64,

    /// Number of consecutive missed detector windows before proposing MarkBrokerDead.
    #[serde(default = "default_dead_broker_consecutive_misses")]
    pub dead_broker_consecutive_misses: u32,

    /// Minimum time between repeated dead-broker proposals for the same broker (ms).
    #[serde(default = "default_dead_broker_proposal_cooldown_ms")]
    pub dead_broker_proposal_cooldown_ms: u64,

    /// Interval for ISR reconciliation ticks (ms).
    #[serde(default = "default_isr_tick_interval_ms")]
    pub isr_tick_interval_ms: u64,

    /// Minimum interval between repeated ISR change proposals per partition (ms).
    #[serde(default = "default_isr_proposal_min_interval_ms")]
    pub isr_proposal_min_interval_ms: u64,
}

impl Default for RaftConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval_ms: default_raft_heartbeat_interval_ms(),
            election_timeout_min_ms: default_election_timeout_min_ms(),
            election_timeout_max_ms: default_election_timeout_max_ms(),
            snapshot_threshold: default_snapshot_threshold(),
            rebalance_timeout_ms: default_rebalance_timeout_ms(),
            broker_heartbeat_propose_interval_ms:
                default_broker_heartbeat_propose_interval_ms(),
            failure_detector_interval_ms: default_failure_detector_interval_ms(),
            dead_broker_threshold_ms: default_dead_broker_threshold_ms(),
            dead_broker_consecutive_misses: default_dead_broker_consecutive_misses(),
            dead_broker_proposal_cooldown_ms: default_dead_broker_proposal_cooldown_ms(),
            isr_tick_interval_ms: default_isr_tick_interval_ms(),
            isr_proposal_min_interval_ms: default_isr_proposal_min_interval_ms(),
        }
    }
}

/// Default rebalance timeout used by both single-node and cluster brokers.
pub const DEFAULT_REBALANCE_TIMEOUT_MS: i64 = 30_000;

fn default_rebalance_timeout_ms() -> i64 {
    DEFAULT_REBALANCE_TIMEOUT_MS
}

fn default_raft_heartbeat_interval_ms() -> u64 {
    1000
}

fn default_election_timeout_min_ms() -> u64 {
    3000
}

fn default_election_timeout_max_ms() -> u64 {
    6000
}

fn default_snapshot_threshold() -> u64 {
    10_000
}

fn default_broker_heartbeat_propose_interval_ms() -> u64 {
    3000
}

fn default_failure_detector_interval_ms() -> u64 {
    5000
}

fn default_dead_broker_threshold_ms() -> u64 {
    15_000
}

fn default_dead_broker_consecutive_misses() -> u32 {
    2
}

fn default_dead_broker_proposal_cooldown_ms() -> u64 {
    30_000
}

fn default_isr_tick_interval_ms() -> u64 {
    500
}

fn default_isr_proposal_min_interval_ms() -> u64 {
    1000
}

fn default_node_id() -> u64 {
    1
}

fn default_api_addr() -> String {
    "127.0.0.1:50051".to_string()
}

fn default_rpc_addr() -> String {
    "127.0.0.1:50052".to_string()
}

fn default_storage_path() -> String {
    "./data/broker-1".to_string()
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_transport() -> String {
    "grpc".to_string()
}

fn default_replication_factor() -> u16 {
    1
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
            api_addr: default_api_addr(),
            rpc_addr: default_rpc_addr(),
            storage_path: default_storage_path(),
            cluster: None,
            raft: None,
            retention: RetentionConfig::default(),
            log_level: default_log_level(),
            transport: default_transport(),
            join_addr: None,
            default_replication_factor: default_replication_factor(),
            membership_api_token: None,
        }
    }

    pub fn is_cluster_mode(&self) -> bool {
        self.cluster
            .as_ref()
            .map(|c| !c.initial_members.is_empty())
            .unwrap_or(false)
    }
}

// Removed openraft conversion as we're using raft-rs now
