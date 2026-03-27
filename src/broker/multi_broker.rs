use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, RwLock};
use tonic::Request;

use crate::api::broker::HeartbeatRequest;
use crate::broker::config::{RaftConfig, RetentionConfig};
use crate::broker::error::{BrokerError, BrokerResult};
use crate::broker::raft::{GroupStatus, RaftNode, RaftStorage};
use crate::broker::raft_network::{PeerInfo, RaftGrpcServer};
use crate::broker::sbe_tcp::server::SbeTcpServer;
use crate::broker::sbe_tcp::transport::SbeTcpTransport;
use crate::broker::storage::{
    validate_topic_name, BrokerInfo, BrokerStorage, ClusterMetadata, GroupMember, StoredMessage,
};
use crate::client::kafka_broker_client::{KafkaBrokerClient, KafkaBrokerClientTrait};

/// Abstraction over gRPC or SBE+TCP Raft server.
pub enum RaftServerHandle {
    Grpc(RaftGrpcServer),
    SbeTcp(SbeTcpServer),
}

impl RaftServerHandle {
    pub async fn serve(self, addr: &str) -> anyhow::Result<()> {
        match self {
            Self::Grpc(s) => s.serve(addr).await,
            Self::SbeTcp(s) => s.serve(addr).await,
        }
    }
}

/// Local (non-replicated) heartbeat timestamps: (group_id, member_id) → last_heartbeat_ms
struct LocalState {
    heartbeats: HashMap<(String, String), i64>,
}

/// Multi-broker implementation using Raft consensus.
pub struct MultiBroker {
    node_id: u64,
    raft_storage: RaftStorage,
    local: Arc<RwLock<LocalState>>,
    broker_host: String,
    broker_port: i32,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

impl MultiBroker {
    pub async fn new(
        node_id: u64,
        peers: HashMap<u64, PeerInfo>,
        storage_path: Option<String>,
        retention: Option<RetentionConfig>,
        raft_config: Option<RaftConfig>,
        transport_kind: &str,
    ) -> Result<(Self, RaftServerHandle), String> {

        let server_handle: RaftServerHandle;
        let raft_node;
        let raft_storage;
        let rebalance_timeout_ms = raft_config
            .as_ref()
            .map(|r| r.rebalance_timeout_ms)
            .unwrap_or(RaftConfig::default().rebalance_timeout_ms);

        if transport_kind == "sbe_tcp" {
            let (step_tx, step_rx) = mpsc::unbounded_channel();
            let sbe_server = SbeTcpServer::new(step_tx.clone());
            let peers_arc = std::sync::Arc::new(std::sync::RwLock::new(peers));
            let transport = SbeTcpTransport::new(node_id, peers_arc.clone());
            let (node, storage) = RaftNode::new_with_transport(
                node_id,
                peers_arc,
                storage_path,
                Box::new(transport),
                step_tx,
                step_rx,
                raft_config,
            )
            .map_err(|e| format!("Failed to create raft node: {:?}", e))?;
            raft_node = node;
            raft_storage = storage;
            server_handle = RaftServerHandle::SbeTcp(sbe_server);
        } else {
            // Default: gRPC
            let (node, storage, grpc_server) = RaftNode::new(node_id, peers, storage_path, raft_config)
                .map_err(|e| format!("Failed to create raft node: {:?}", e))?;
            raft_node = node;
            raft_storage = storage;
            server_handle = RaftServerHandle::Grpc(grpc_server);
        }

        tokio::spawn(raft_node.run());

        // Retention task
        if let Some(ret) = retention {
            if ret.retention_ms.is_some() || ret.max_messages_per_partition.is_some() {
                let storage = raft_storage.clone();
                tokio::spawn(async move {
                    let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
                    loop {
                        interval.tick().await;
                        if !storage.is_leader() {
                            continue;
                        }
                        let now = now_ms();
                        let data = storage.read_data().await;
                        let mut to_truncate: Vec<(String, i32, i64)> = Vec::new();
                        for ((topic, partition), log) in &data.messages {
                            let mut before_offset = None;
                            if let Some(max) = ret.max_messages_per_partition {
                                if log.len() > max {
                                    let drop = log.len() - max;
                                    let candidate = if drop < log.len() {
                                        log[drop].offset
                                    } else {
                                        log.last().map_or(0, |m| m.offset + 1)
                                    };
                                    before_offset = Some(candidate);
                                }
                            }
                            if let Some(ms) = ret.retention_ms {
                                let cutoff = now - ms as i64;
                                let drop = log.partition_point(|m| m.timestamp_ms < cutoff);
                                if drop > 0 {
                                    let candidate = if drop < log.len() {
                                        log[drop].offset
                                    } else {
                                        log.last().map_or(0, |m| m.offset + 1)
                                    };
                                    before_offset = Some(
                                        before_offset.map_or(candidate, |b: i64| b.max(candidate)),
                                    );
                                }
                            }
                            if let Some(off) = before_offset {
                                to_truncate.push((topic.clone(), *partition, off));
                            }
                        }
                        drop(data);
                        for (topic, partition, before_offset) in to_truncate {
                            let _ = storage
                                .propose_truncate(topic, partition, before_offset)
                                .await;
                        }
                    }
                });
            }
        }

        // Heartbeat expiry + rebalance finalization task
        let local = Arc::new(RwLock::new(LocalState {
            heartbeats: HashMap::new(),
        }));
        {
            let storage = raft_storage.clone();
            let local_clone = local.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
                loop {
                    interval.tick().await;
                    if !storage.is_leader() {
                        continue;
                    }
                    let now = now_ms();
                    let data = storage.read_data().await;
                    let mut to_expire: Vec<(String, Vec<String>)> = Vec::new();
                    let mut to_finalize: Vec<String> = Vec::new();

                    for (group_id, group) in &data.groups {
                        let heartbeats = local_clone.read().await;
                        let expired: Vec<String> = group
                            .members
                            .iter()
                            .filter(|m| {
                                let last = heartbeats
                                    .heartbeats
                                    .get(&(group_id.clone(), m.member_id.clone()))
                                    .copied()
                                    .unwrap_or(0);
                                // Grace period: if no heartbeat recorded yet (0), use now (just joined)
                                last > 0 && now - last > group.session_timeout_ms
                            })
                            .map(|m| m.member_id.clone())
                            .collect();
                        if !expired.is_empty() {
                            to_expire.push((group_id.clone(), expired));
                        }
                        if group.status == GroupStatus::PreparingRebalance
                            && group.rebalance_started_ms > 0
                            && now - group.rebalance_started_ms > rebalance_timeout_ms
                        {
                            to_finalize.push(group_id.clone());
                        }
                    }
                    drop(data);

                    for (group_id, expired_ids) in to_expire {
                        let _ = storage.propose_group_expire(group_id, expired_ids).await;
                    }
                    for group_id in to_finalize {
                        let _ = storage.propose_group_finalize(group_id).await;
                    }
                }
            });
        }

        let broker = Self {
            node_id,
            raft_storage,
            local,
            broker_host: "127.0.0.1".to_string(),
            broker_port: 9092 + node_id as i32,
        };

        Ok((broker, server_handle))
    }

    async fn forward_heartbeat_to_leader(
        &self,
        group_id: &str,
        member_id: &str,
    ) -> BrokerResult<()> {
        let leader_addr = self
            .raft_storage
            .leader_api_addr()
            .ok_or_else(|| BrokerError::NotLeader { leader_addr: None })?;
        let client =
            KafkaBrokerClient::new(&leader_addr)
                .await
                .map_err(|_| BrokerError::NotLeader {
                    leader_addr: Some(leader_addr.clone()),
                })?;
        let req = Request::new(HeartbeatRequest {
            group_id: group_id.to_string(),
            generation_id: 0,
            member_id: member_id.to_string(),
        });
        let resp = client.heartbeat(req).await.map_err(|e| {
            BrokerError::Internal(format!(
                "Failed to forward heartbeat to leader {}: {}",
                leader_addr, e
            ))
        })?;
        match resp.error_code {
            0 => Ok(()),
            27 => Err(BrokerError::RebalanceInProgress),
            code => Err(BrokerError::Internal(format!(
                "Heartbeat error_code={} from leader {}",
                code, leader_addr
            ))),
        }
    }
}

#[async_trait]
impl BrokerStorage for MultiBroker {
    async fn create_topic(&self, topic: &str, num_partitions: i32) -> BrokerResult<()> {
        validate_topic_name(topic)?;
        self.raft_storage
            .propose_create_topic(topic.to_string(), num_partitions)
            .await
            .map_err(BrokerError::from)
    }

    async fn get_topics(&self) -> Vec<String> {
        let data = self.raft_storage.read_data().await;
        data.topics.keys().cloned().collect()
    }

    async fn get_topic_partitions(&self, topic: &str) -> Option<Vec<i32>> {
        let data = self.raft_storage.read_data().await;
        data.topics.get(topic).map(|&n| (0..n).collect())
    }

    async fn produce_message(
        &self,
        topic: &str,
        partition: i32,
        key: Option<Vec<u8>>,
        value: Vec<u8>,
    ) -> BrokerResult<i64> {
        validate_topic_name(topic)?;
        self.raft_storage
            .propose_produce(topic.to_string(), partition, key, value)
            .await
            .map_err(BrokerError::from)
    }

    async fn fetch_messages(
        &self,
        topic: &str,
        partition: i32,
        offset: i64,
        max_bytes: i32,
    ) -> BrokerResult<Vec<StoredMessage>> {
        let data = self.raft_storage.read_data().await;
        let Some(log) = data.messages.get(&(topic.to_owned(), partition)) else {
            return Ok(vec![]);
        };
        let start = log.partition_point(|m| m.offset < offset);
        if start >= log.len() {
            return Ok(vec![]);
        }
        let mut result = Vec::new();
        let mut total_bytes = 0usize;
        for msg in &log[start..] {
            let size = msg.value.len() + msg.key.as_ref().map_or(0, |k| k.len());
            if total_bytes + size > max_bytes as usize && !result.is_empty() {
                break;
            }
            result.push(StoredMessage {
                offset: msg.offset,
                key: msg.key.clone(),
                value: msg.value.clone(),
                timestamp_ms: msg.timestamp_ms,
            });
            total_bytes += size;
        }
        Ok(result)
    }

    async fn get_partition_offset(
        &self,
        topic: &str,
        partition: i32,
        time: i64,
    ) -> BrokerResult<Vec<i64>> {
        let data = self.raft_storage.read_data().await;
        let log = data.messages.get(&(topic.to_owned(), partition));
        // Use the high-watermark as the floor for "next offset" so callers see the correct
        // value even when the log has been fully truncated by retention.
        let hwm = data
            .next_offsets
            .get(&(topic.to_owned(), partition))
            .copied()
            .unwrap_or(0);
        let next_offset = log.and_then(|l| l.last()).map_or(hwm, |m| m.offset + 1);
        let offset = match time {
            -1 => vec![next_offset],
            -2 => vec![log.and_then(|l| l.first()).map_or(0, |m| m.offset)],
            ts => {
                let idx = log.map_or(0, |l| l.partition_point(|m| m.timestamp_ms < ts));
                vec![log
                    .and_then(|l| l.get(idx))
                    .map_or(next_offset, |m| m.offset)]
            }
        };
        Ok(offset)
    }

    async fn commit_offset(
        &self,
        group: &str,
        topic: &str,
        partition: i32,
        offset: i64,
        _metadata: String,
    ) -> BrokerResult<()> {
        self.raft_storage
            .propose_commit_offset(group.to_string(), topic.to_string(), partition, offset)
            .await
            .map_err(BrokerError::from)
    }

    async fn fetch_offset(
        &self,
        group: &str,
        topic: &str,
        partition: i32,
    ) -> BrokerResult<(i64, String)> {
        let data = self.raft_storage.read_data().await;
        let offset = data
            .offsets
            .get(&(group.to_owned(), topic.to_owned(), partition))
            .copied()
            .unwrap_or(-1);
        Ok((offset, String::new()))
    }

    async fn get_coordinator_info(&self) -> (i32, String, i32) {
        (
            self.node_id as i32,
            self.broker_host.clone(),
            self.broker_port,
        )
    }

    async fn join_group(
        &self,
        group_id: &str,
        member_id: &str,
        _protocol_type: &str,
        protocol_metadata: &[u8],
        session_timeout_ms: i64,
    ) -> BrokerResult<(i32, String, String, Vec<GroupMember>)> {
        let topic = std::str::from_utf8(protocol_metadata)
            .unwrap_or("")
            .to_string();

        // Record heartbeat on join
        {
            let mut local = self.local.write().await;
            local
                .heartbeats
                .insert((group_id.to_string(), member_id.to_string()), now_ms());
        }

        self.raft_storage
            .propose_group_join(
                group_id.to_string(),
                member_id.to_string(),
                topic,
                protocol_metadata.to_vec(),
                session_timeout_ms,
            )
            .await
            .map_err(BrokerError::from)?;

        // Read resulting group state
        let data = self.raft_storage.read_data().await;
        let group = data
            .groups
            .get(group_id)
            .ok_or_else(|| BrokerError::NotFound("Group not found after join".to_string()))?;
        let members: Vec<GroupMember> = group
            .members
            .iter()
            .map(|m| GroupMember {
                member_id: m.member_id.clone(),
                metadata: m.metadata.clone(),
            })
            .collect();
        Ok((
            group.generation_id,
            group.leader_id.clone(),
            member_id.to_string(),
            members,
        ))
    }

    async fn sync_group(
        &self,
        group_id: &str,
        _generation_id: i32,
        member_id: &str,
    ) -> BrokerResult<Vec<u8>> {
        let data = self.raft_storage.read_data().await;
        let group = data
            .groups
            .get(group_id)
            .ok_or_else(|| BrokerError::NotFound("Group not found".to_string()))?;
        if group.status == GroupStatus::PreparingRebalance {
            return Err(BrokerError::RebalanceInProgress);
        }
        let assigned = group
            .assignments
            .get(member_id)
            .cloned()
            .unwrap_or_default();
        Ok(bincode::serialize(&assigned).unwrap_or_default())
    }

    async fn heartbeat(
        &self,
        group_id: &str,
        _generation_id: i32,
        member_id: &str,
    ) -> BrokerResult<()> {
        // Followers forward heartbeats to the active leader so leader-side expiry
        // logic always observes the latest member liveness.
        if !self.raft_storage.is_leader() {
            return self.forward_heartbeat_to_leader(group_id, member_id).await;
        }

        // Update local heartbeat timestamp
        {
            let mut local = self.local.write().await;
            local
                .heartbeats
                .insert((group_id.to_string(), member_id.to_string()), now_ms());
        }

        let data = self.raft_storage.read_data().await;
        let group = data
            .groups
            .get(group_id)
            .ok_or_else(|| BrokerError::NotFound(format!("Unknown group: {}", group_id)))?;
        if !group.members.iter().any(|m| m.member_id == member_id) {
            return Err(BrokerError::NotFound(format!(
                "Unknown member '{}' in group '{}'",
                member_id, group_id
            )));
        }
        if group.status == GroupStatus::PreparingRebalance {
            return Err(BrokerError::RebalanceInProgress);
        }
        Ok(())
    }

    async fn get_cluster_metadata(&self) -> ClusterMetadata {
        let peers = self.raft_storage.peers_snapshot();
        let leader_id = self.raft_storage.current_leader_id();
        let brokers = peers
            .into_iter()
            .map(|(id, api_addr)| BrokerInfo { node_id: id, api_addr })
            .collect();
        ClusterMetadata { brokers, leader_id }
    }

    async fn add_node(
        &self,
        node_id: u64,
        api_addr: String,
        rpc_addr: String,
    ) -> BrokerResult<()> {
        self.raft_storage
            .propose_add_node(node_id, api_addr, rpc_addr)
            .await
            .map_err(BrokerError::from)
    }

    async fn remove_node(&self, node_id: u64) -> BrokerResult<()> {
        self.raft_storage
            .propose_remove_node(node_id)
            .await
            .map_err(BrokerError::from)
    }

    async fn leave_group(&self, group_id: &str, member_id: &str) -> BrokerResult<()> {
        // Remove heartbeat entry
        {
            let mut local = self.local.write().await;
            local
                .heartbeats
                .remove(&(group_id.to_string(), member_id.to_string()));
        }

        self.raft_storage
            .propose_group_leave(group_id.to_string(), member_id.to_string())
            .await
            .map_err(BrokerError::from)
    }
}
