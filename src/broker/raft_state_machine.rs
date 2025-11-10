use async_trait::async_trait;
use openraft::{
    storage::RaftStateMachine,
    Entry, EntryPayload, LogId, RaftSnapshotBuilder, SnapshotMeta, StoredMembership,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;

use crate::broker::raft_types::{
    BrokerNodeId, BrokerNode, BrokerRequest, BrokerResponse, BrokerStateMachineData,
    ConsumerGroupState, GroupMember, TopicConfig, TypeConfig,
};

/// State machine for the broker that handles Raft log entries
pub struct BrokerStateMachine {
    /// The application state
    pub data: Arc<RwLock<BrokerStateMachineData>>,

    /// Last applied log
    last_applied_log: Arc<RwLock<Option<LogId<BrokerNodeId>>>>,

    /// Last membership config
    last_membership: Arc<RwLock<StoredMembership<BrokerNodeId, BrokerNode>>>,

    /// Snapshot index
    snapshot_idx: Arc<Mutex<u64>>,
}

impl BrokerStateMachine {
    pub fn new() -> Self {
        Self {
            data: Arc::new(RwLock::new(BrokerStateMachineData::default())),
            last_applied_log: Arc::new(RwLock::new(None)),
            last_membership: Arc::new(RwLock::new(StoredMembership::default())),
            snapshot_idx: Arc::new(Mutex::new(0)),
        }
    }

    /// Apply a broker request to the state machine
    async fn apply_request(&self, request: &BrokerRequest) -> BrokerResponse {
        let mut data = self.data.write().await;

        match request {
            BrokerRequest::Produce { topic, partition, message } => {
                // Get or create topic
                let topic_messages = data.messages
                    .entry(topic.clone())
                    .or_insert_with(BTreeMap::new);
                
                // Get or create partition
                let partition_messages = topic_messages
                    .entry(*partition)
                    .or_insert_with(Vec::new);
                
                // Add message with offset
                let offset = partition_messages.len() as i64;
                partition_messages.push((offset, message.clone()));
                
                BrokerResponse::Produce { offset }
            }
            
            BrokerRequest::CommitOffset { group, topic, partition, offset, metadata } => {
                let group_data = data.offsets
                    .entry(group.clone())
                    .or_insert_with(BTreeMap::new);
                
                let topic_data = group_data
                    .entry(topic.clone())
                    .or_insert_with(BTreeMap::new);
                
                topic_data.insert(*partition, (*offset, metadata.clone()));
                
                BrokerResponse::CommitOffset
            }
            
            BrokerRequest::JoinGroup { group_id, member_id, protocol_type } => {
                let group = data.groups
                    .entry(group_id.clone())
                    .or_insert_with(|| ConsumerGroupState {
                        generation_id: 0,
                        leader_id: member_id.clone(),
                        protocol: protocol_type.clone(),
                        members: Vec::new(),
                    });
                
                // Check if member already exists
                if !group.members.iter().any(|m| m.member_id == *member_id) {
                    group.members.push(GroupMember {
                        member_id: member_id.clone(),
                        metadata: Vec::new(),
                    });
                    group.generation_id += 1;
                }
                
                BrokerResponse::JoinGroup {
                    generation_id: group.generation_id,
                    leader_id: group.leader_id.clone(),
                    member_id: member_id.clone(),
                }
            }
            
            BrokerRequest::LeaveGroup { group_id, member_id } => {
                if let Some(group) = data.groups.get_mut(group_id) {
                    group.members.retain(|m| m.member_id != *member_id);
                    if group.members.is_empty() {
                        data.groups.remove(group_id);
                    } else {
                        group.generation_id += 1;
                    }
                }
                BrokerResponse::LeaveGroup
            }
            
            BrokerRequest::CreateTopic { topic, partitions, replication_factor } => {
                data.topics.insert(topic.clone(), TopicConfig {
                    partitions: *partitions,
                    replication_factor: *replication_factor,
                });
                
                // Initialize empty partitions
                let topic_messages = data.messages
                    .entry(topic.clone())
                    .or_insert_with(BTreeMap::new);
                
                for i in 0..*partitions {
                    topic_messages.entry(i).or_insert_with(Vec::new);
                }
                
                BrokerResponse::CreateTopic
            }
            
            BrokerRequest::DeleteTopic { topic } => {
                data.topics.remove(topic);
                data.messages.remove(topic);
                
                // Remove all offsets for this topic
                for group_offsets in data.offsets.values_mut() {
                    group_offsets.remove(topic);
                }
                
                BrokerResponse::DeleteTopic
            }
            
            BrokerRequest::GroupHeartbeat { group_id, generation_id, member_id } => {
                if let Some(group) = data.groups.get(group_id) {
                    if group.generation_id == *generation_id 
                        && group.members.iter().any(|m| m.member_id == *member_id) {
                        BrokerResponse::GroupHeartbeat
                    } else {
                        BrokerResponse::Error("Invalid generation or unknown member".to_string())
                    }
                } else {
                    BrokerResponse::Error("Unknown group".to_string())
                }
            }
        }
    }
}

#[async_trait]
impl RaftStateMachine<TypeConfig> for BrokerStateMachine {
    type SnapshotBuilder = Self;

    async fn applied_state(
        &mut self,
    ) -> Result<(Option<LogId<BrokerNodeId>>, StoredMembership<BrokerNodeId, BrokerNode>), openraft::StorageError<BrokerNodeId>> {
        let last_applied = *self.last_applied_log.read().await;
        let last_membership = self.last_membership.read().await.clone();
        Ok((last_applied, last_membership))
    }

    async fn apply<I>(&mut self, entries: I) -> Result<Vec<BrokerResponse>, openraft::StorageError<BrokerNodeId>>
    where
        I: IntoIterator<Item = Entry<TypeConfig>>,
    {
        let mut responses = Vec::new();
        
        for entry in entries {
            tracing::debug!("Applying log entry: {:?}", entry.log_id);
            
            *self.last_applied_log.write().await = Some(entry.log_id);

            match entry.payload {
                EntryPayload::Normal(req) => {
                    let response = self.apply_request(&req).await;
                    responses.push(response);
                }
                EntryPayload::Membership(mem) => {
                    *self.last_membership.write().await = StoredMembership::new(Some(entry.log_id), mem);
                }
                EntryPayload::Blank => {}
            }
        }

        Ok(responses)
    }

    async fn get_snapshot_builder(&mut self) -> Self::SnapshotBuilder {
        self.clone()
    }

    async fn begin_receiving_snapshot(&mut self) -> Result<Box<Cursor<Vec<u8>>>, openraft::StorageError<BrokerNodeId>> {
        Ok(Box::new(Cursor::new(Vec::new())))
    }

    async fn install_snapshot(
        &mut self,
        meta: &SnapshotMeta<BrokerNodeId, crate::broker::raft_types::BrokerNode>,
        snapshot: Box<Cursor<Vec<u8>>>,
    ) -> Result<(), openraft::StorageError<BrokerNodeId>> {
        tracing::info!("Installing snapshot: {:?}", meta);

        let new_snapshot = SnapshotData {
            meta: meta.clone(),
            data: snapshot.into_inner(),
        };

        // Deserialize snapshot data
        let data: BrokerStateMachineData = serde_json::from_slice(&new_snapshot.data)
            .map_err(|e| openraft::StorageError::from_io_error(
                openraft::ErrorSubject::Snapshot(Some(meta.signature())),
                openraft::ErrorVerb::Read,
                std::io::Error::new(std::io::ErrorKind::InvalidData, e),
            ))?;

        // Update state
        *self.data.write().await = data;
        *self.last_applied_log.write().await = Some(meta.last_log_id.unwrap_or_default());
        *self.last_membership.write().await = meta.last_membership.clone();

        Ok(())
    }

    async fn get_current_snapshot(&mut self) -> Result<Option<openraft::storage::Snapshot<TypeConfig>>, openraft::StorageError<BrokerNodeId>> {
        // For simplicity, we don't store snapshots yet
        Ok(None)
    }
}

#[async_trait]
impl RaftSnapshotBuilder<TypeConfig> for BrokerStateMachine {
    async fn build_snapshot(&mut self) -> Result<openraft::storage::Snapshot<TypeConfig>, openraft::StorageError<BrokerNodeId>> {
        let data = self.data.read().await;
        let last_applied = self.last_applied_log.read().await;
        let last_membership = self.last_membership.read().await;

        let data_bytes = serde_json::to_vec(&*data)
            .map_err(|e| openraft::StorageError::from_io_error(
                openraft::ErrorSubject::Snapshot(None),
                openraft::ErrorVerb::Write,
                std::io::Error::new(std::io::ErrorKind::InvalidData, e),
            ))?;

        let snapshot_idx = {
            let mut idx = self.snapshot_idx.lock().unwrap();
            *idx += 1;
            *idx
        };

        let snapshot = SnapshotData {
            meta: SnapshotMeta {
                last_log_id: *last_applied,
                last_membership: last_membership.clone(),
                snapshot_id: format!("{}", snapshot_idx),
            },
            data: data_bytes,
        };

        Ok(openraft::storage::Snapshot {
            meta: snapshot.meta.clone(),
            snapshot: Box::new(Cursor::new(snapshot.data)),
        })
    }
}

impl Clone for BrokerStateMachine {
    fn clone(&self) -> Self {
        Self {
            data: self.data.clone(),
            last_applied_log: self.last_applied_log.clone(),
            last_membership: self.last_membership.clone(),
            snapshot_idx: self.snapshot_idx.clone(),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct SnapshotData {
    meta: SnapshotMeta<BrokerNodeId, BrokerNode>,
    data: Vec<u8>,
}
