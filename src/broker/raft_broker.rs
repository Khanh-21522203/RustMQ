use async_trait::async_trait;
use openraft::{Config, Raft, RaftMetrics};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tonic::transport::Server;

use crate::broker::raft_network::{BrokerRaftNetwork, create_raft_grpc_server};
use crate::broker::raft_storage::BrokerRaftStorage;
use crate::broker::raft_types::{
    BrokerNode, BrokerNodeId, BrokerRequest, BrokerResponse,
    BrokerStateMachineData, TypeConfig, create_raft_config,
};
use crate::broker::storage::{BrokerStorage, GroupMember as StorageGroupMember};

pub type BrokerRaft = Raft<TypeConfig>;

/// Raft-based broker implementation
pub struct RaftBroker {
    /// Node ID
    node_id: BrokerNodeId,
    
    /// Raft instance
    raft: Arc<BrokerRaft>,
    
    /// Storage
    storage: Arc<BrokerRaftStorage>,
    
    /// Metrics receiver
    metrics_rx: mpsc::UnboundedReceiver<RaftMetrics<BrokerNodeId, BrokerNode>>,
    
    /// Cluster nodes
    nodes: Arc<RwLock<BTreeMap<BrokerNodeId, BrokerNode>>>,
    
    /// RPC address for this node
    rpc_addr: String,
    
    /// API address for this node  
    api_addr: String,
}

impl RaftBroker {
    pub async fn new(
        node_id: BrokerNodeId,
        storage_path: String,
        rpc_addr: String,
        api_addr: String,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        // Create storage
        let storage = Arc::new(BrokerRaftStorage::new(storage_path)?);
        
        // Create network
        let network = BrokerRaftNetwork::new(node_id);
        
        // Create Raft config
        let config = Arc::new(create_raft_config());
        
        // Create Raft instance
        let raft = Raft::new(
            node_id,
            config,
            network,
            storage.clone(),
        ).await?;
        
        let (metrics_tx, metrics_rx) = mpsc::unbounded_channel();
        let _ = raft.metrics().add_watcher(metrics_tx);
        
        Ok(Self {
            node_id,
            raft: Arc::new(raft),
            storage,
            metrics_rx,
            nodes: Arc::new(RwLock::new(BTreeMap::new())),
            rpc_addr,
            api_addr,
        })
    }
    
    /// Start the Raft gRPC server
    pub async fn start_rpc_server(&self) -> Result<(), Box<dyn std::error::Error>> {
        let addr = self.rpc_addr.parse()?;
        let service = create_raft_grpc_server(self.raft.clone());
        
        tokio::spawn(async move {
            Server::builder()
                .add_service(service)
                .serve(addr)
                .await
                .expect("Failed to start Raft RPC server");
        });
        
        Ok(())
    }
    
    /// Initialize a new cluster
    pub async fn init_cluster(&self) -> Result<(), Box<dyn std::error::Error>> {
        let mut nodes = BTreeMap::new();
        nodes.insert(self.node_id, BrokerNode {
            rpc_addr: self.rpc_addr.clone(),
            api_addr: self.api_addr.clone(),
        });
        
        self.raft.initialize(nodes).await?;
        Ok(())
    }
    
    /// Add a node to the cluster
    pub async fn add_node(&self, node_id: BrokerNodeId, node: BrokerNode) -> Result<(), Box<dyn std::error::Error>> {
        self.raft.add_learner(node_id, node.clone(), true).await?;
        self.raft.change_membership(&[node_id], false).await?;
        
        let mut nodes = self.nodes.write().await;
        nodes.insert(node_id, node);
        
        Ok(())
    }
    
    /// Remove a node from the cluster
    pub async fn remove_node(&self, node_id: BrokerNodeId) -> Result<(), Box<dyn std::error::Error>> {
        let nodes = self.nodes.read().await;
        let remaining_nodes: Vec<BrokerNodeId> = nodes.keys()
            .filter(|&&id| id != node_id)
            .copied()
            .collect();
        
        self.raft.change_membership(&remaining_nodes, false).await?;
        
        drop(nodes);
        let mut nodes = self.nodes.write().await;
        nodes.remove(&node_id);
        
        Ok(())
    }
    
    /// Check if this node is the leader
    pub async fn is_leader(&self) -> bool {
        self.raft.is_leader().await
    }
    
    /// Get current leader
    pub async fn current_leader(&self) -> Option<BrokerNodeId> {
        self.raft.current_leader().await
    }
    
    /// Write to Raft (must be called on leader)
    pub async fn write(&self, request: BrokerRequest) -> Result<BrokerResponse, Box<dyn std::error::Error>> {
        let response = self.raft.client_write(request).await?;
        Ok(response.data)
    }
    
    /// Read from state machine
    pub async fn read(&self) -> BrokerStateMachineData {
        let state_machine = &self.storage.state_machine;
        let data = state_machine.read().await.data.read().await;
        data.clone()
    }
    
    /// Forward request to leader
    pub async fn forward_to_leader(&self, request: BrokerRequest) -> Result<BrokerResponse, Box<dyn std::error::Error>> {
        if let Some(leader_id) = self.current_leader().await {
            if leader_id == self.node_id {
                // We are the leader
                return self.write(request).await;
            }
            
            // Forward to leader
            let nodes = self.nodes.read().await;
            if let Some(leader_node) = nodes.get(&leader_id) {
                // In a real implementation, you would use gRPC to forward the request
                // For now, return an error indicating to retry with the leader
                return Err(format!("Not leader, forward to node {} at {}", leader_id, leader_node.api_addr).into());
            }
        }
        
        Err("No leader available".into())
    }
}

/// Adapter to use RaftBroker as BrokerStorage
#[async_trait]
impl BrokerStorage for RaftBroker {
    async fn get_topics(&self) -> Vec<String> {
        let data = self.read().await;
        data.topics.keys().cloned().collect()
    }
    
    async fn get_topic_partitions(&self, topic: &str) -> Option<Vec<i32>> {
        let data = self.read().await;
        data.topics.get(topic).map(|config| {
            (0..config.partitions).collect()
        })
    }
    
    async fn produce_message(&mut self, topic: &str, partition: i32, message: Vec<u8>) -> Result<i64, String> {
        let request = BrokerRequest::Produce {
            topic: topic.to_string(),
            partition,
            message,
        };
        
        match self.forward_to_leader(request).await {
            Ok(BrokerResponse::Produce { offset }) => Ok(offset),
            Ok(BrokerResponse::Error(e)) => Err(e),
            Ok(_) => Err("Unexpected response".to_string()),
            Err(e) => Err(e.to_string()),
        }
    }
    
    async fn fetch_messages(&self, topic: &str, partition: i32, offset: i64, max_bytes: i32) -> Result<(Vec<u8>, i64), String> {
        let data = self.read().await;
        
        if let Some(topic_messages) = data.messages.get(topic) {
            if let Some(partition_messages) = topic_messages.get(&partition) {
                let mut result = Vec::new();
                let mut high_watermark = offset;
                
                for (msg_offset, msg_data) in partition_messages {
                    if *msg_offset >= offset && result.len() < max_bytes as usize {
                        result.extend_from_slice(msg_data);
                        high_watermark = *msg_offset + 1;
                    }
                }
                
                high_watermark = high_watermark.max(partition_messages.len() as i64);
                return Ok((result, high_watermark));
            }
        }
        
        Err("Topic or partition not found".to_string())
    }
    
    async fn get_partition_offset(&self, topic: &str, partition: i32, time: i64) -> Result<Vec<i64>, String> {
        let data = self.read().await;
        
        if let Some(topic_messages) = data.messages.get(topic) {
            if let Some(partition_messages) = topic_messages.get(&partition) {
                let offset = match time {
                    -1 => vec![partition_messages.len() as i64], // Latest
                    -2 => vec![0],                                // Earliest
                    _ => vec![partition_messages.len() as i64],   // Default to latest
                };
                return Ok(offset);
            }
        }
        
        Err("Topic or partition not found".to_string())
    }
    
    async fn commit_offset(&mut self, group: &str, topic: &str, partition: i32, offset: i64, metadata: String) -> Result<(), String> {
        let request = BrokerRequest::CommitOffset {
            group: group.to_string(),
            topic: topic.to_string(),
            partition,
            offset,
            metadata,
        };
        
        match self.forward_to_leader(request).await {
            Ok(BrokerResponse::CommitOffset) => Ok(()),
            Ok(BrokerResponse::Error(e)) => Err(e),
            Ok(_) => Err("Unexpected response".to_string()),
            Err(e) => Err(e.to_string()),
        }
    }
    
    async fn fetch_offset(&self, group: &str, topic: &str, partition: i32) -> Result<(i64, String), String> {
        let data = self.read().await;
        
        if let Some(group_offsets) = data.offsets.get(group) {
            if let Some(topic_offsets) = group_offsets.get(topic) {
                if let Some((offset, metadata)) = topic_offsets.get(&partition) {
                    return Ok((*offset, metadata.clone()));
                }
            }
        }
        
        // Return -1 to indicate no committed offset
        Ok((-1, String::new()))
    }
    
    async fn get_coordinator_info(&self) -> (i32, String, i32) {
        // Return current leader info if available
        if let Some(leader_id) = self.current_leader().await {
            let nodes = self.nodes.read().await;
            if let Some(leader_node) = nodes.get(&leader_id) {
                let parts: Vec<&str> = leader_node.api_addr.split(':').collect();
                let host = parts[0].to_string();
                let port = parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(9092);
                return (leader_id as i32, host, port);
            }
        }
        
        // Fallback to self
        let parts: Vec<&str> = self.api_addr.split(':').collect();
        let host = parts[0].to_string();
        let port = parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(9092);
        (self.node_id as i32, host, port)
    }
    
    async fn join_group(&mut self, group_id: &str, member_id: &str, protocol_type: &str) -> Result<(i32, String, String, Vec<StorageGroupMember>), String> {
        let request = BrokerRequest::JoinGroup {
            group_id: group_id.to_string(),
            member_id: member_id.to_string(),
            protocol_type: protocol_type.to_string(),
        };
        
        match self.forward_to_leader(request).await {
            Ok(BrokerResponse::JoinGroup { generation_id, leader_id, member_id }) => {
                // Get current members
                let data = self.read().await;
                let members = if let Some(group) = data.groups.get(group_id) {
                    group.members.iter().map(|m| StorageGroupMember {
                        member_id: m.member_id.clone(),
                        metadata: m.metadata.clone(),
                    }).collect()
                } else {
                    Vec::new()
                };
                
                Ok((generation_id, leader_id, member_id, members))
            }
            Ok(BrokerResponse::Error(e)) => Err(e),
            Ok(_) => Err("Unexpected response".to_string()),
            Err(e) => Err(e.to_string()),
        }
    }
    
    async fn sync_group(&mut self, _group_id: &str, _generation_id: i32, _member_id: &str) -> Result<Vec<u8>, String> {
        // Return empty assignment for now
        Ok(Vec::new())
    }
    
    async fn heartbeat(&mut self, group_id: &str, generation_id: i32, member_id: &str) -> Result<(), String> {
        let request = BrokerRequest::GroupHeartbeat {
            group_id: group_id.to_string(),
            generation_id,
            member_id: member_id.to_string(),
        };
        
        match self.forward_to_leader(request).await {
            Ok(BrokerResponse::GroupHeartbeat) => Ok(()),
            Ok(BrokerResponse::Error(e)) => Err(e),
            Ok(_) => Err("Unexpected response".to_string()),
            Err(e) => Err(e.to_string()),
        }
    }
    
    async fn leave_group(&mut self, group_id: &str, member_id: &str) -> Result<(), String> {
        let request = BrokerRequest::LeaveGroup {
            group_id: group_id.to_string(),
            member_id: member_id.to_string(),
        };
        
        match self.forward_to_leader(request).await {
            Ok(BrokerResponse::LeaveGroup) => Ok(()),
            Ok(BrokerResponse::Error(e)) => Err(e),
            Ok(_) => Err("Unexpected response".to_string()),
            Err(e) => Err(e.to_string()),
        }
    }
}
