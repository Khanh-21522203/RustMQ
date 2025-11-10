use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::broker::simple_raft::SimpleRaftStorage;
use crate::broker::storage::{BrokerStorage, GroupMember};

/// Multi-broker implementation using simple Raft
pub struct MultiBroker {
    node_id: u64,
    raft_storage: Arc<SimpleRaftStorage>,
    // Temporary in-memory storage for read operations
    local_storage: Arc<Mutex<LocalStorage>>,
}

struct LocalStorage {
    topics: Vec<String>,
    partitions: std::collections::HashMap<String, Vec<i32>>,
    groups: std::collections::HashMap<String, GroupInfo>,
}

struct GroupInfo {
    generation_id: i32,
    leader_id: String,
    members: Vec<GroupMember>,
}

impl Default for LocalStorage {
    fn default() -> Self {
        let mut storage = Self {
            topics: vec!["test-topic".to_string()],
            partitions: std::collections::HashMap::new(),
            groups: std::collections::HashMap::new(),
        };
        storage.partitions.insert("test-topic".to_string(), vec![0, 1]);
        storage
    }
}

impl MultiBroker {
    pub fn new(node_id: u64, peers: Vec<u64>) -> Result<Self, String> {
        let raft_storage = Arc::new(SimpleRaftStorage::new(node_id, peers)
            .map_err(|e| format!("Failed to create raft storage: {}", e))?);
        let local_storage = Arc::new(Mutex::new(LocalStorage::default()));

        Ok(Self {
            node_id,
            raft_storage,
            local_storage,
        })
    }
}

#[async_trait]
impl BrokerStorage for MultiBroker {
    async fn get_topics(&self) -> Vec<String> {
        self.local_storage.lock().await.topics.clone()
    }

    async fn get_topic_partitions(&self, topic: &str) -> Option<Vec<i32>> {
        self.local_storage.lock().await.partitions.get(topic).cloned()
    }

    async fn produce_message(&mut self, topic: &str, _partition: i32, message: Vec<u8>) -> Result<i64, String> {
        if !self.raft_storage.is_leader() {
            return Err("Not leader. Please retry with leader node".to_string());
        }

        self.raft_storage.produce(topic.to_string(), message).await?;
        
        // Return a simple offset based on current message count
        let messages = self.raft_storage.get_messages(topic);
        Ok(messages.len() as i64 - 1)
    }

    async fn fetch_messages(&self, topic: &str, _partition: i32, offset: i64, max_bytes: i32) -> Result<(Vec<u8>, i64), String> {
        let messages = self.raft_storage.get_messages(topic);
        
        let mut result = Vec::new();
        let mut bytes_count = 0;
        
        for (idx, msg) in messages.iter().enumerate() {
            if idx as i64 >= offset && bytes_count < max_bytes as usize {
                result.extend_from_slice(msg);
                bytes_count += msg.len();
            }
        }
        
        let high_watermark = messages.len() as i64;
        Ok((result, high_watermark))
    }

    async fn get_partition_offset(&self, topic: &str, _partition: i32, time: i64) -> Result<Vec<i64>, String> {
        let messages = self.raft_storage.get_messages(topic);
        let offset = match time {
            -1 => vec![messages.len() as i64], // Latest
            -2 => vec![0],                      // Earliest
            _ => vec![messages.len() as i64],   // Default to latest
        };
        Ok(offset)
    }

    async fn commit_offset(&mut self, group: &str, _topic: &str, _partition: i32, offset: i64, _metadata: String) -> Result<(), String> {
        if !self.raft_storage.is_leader() {
            return Err("Not leader. Please retry with leader node".to_string());
        }

        self.raft_storage.commit_offset(group.to_string(), offset).await
    }

    async fn fetch_offset(&self, group: &str, _topic: &str, _partition: i32) -> Result<(i64, String), String> {
        let offset = self.raft_storage.get_offset(group).unwrap_or(-1);
        Ok((offset, String::new()))
    }

    async fn get_coordinator_info(&self) -> (i32, String, i32) {
        (self.node_id as i32, "127.0.0.1".to_string(), 9092 + self.node_id as i32)
    }

    async fn join_group(&mut self, group_id: &str, member_id: &str, _protocol_type: &str) -> Result<(i32, String, String, Vec<GroupMember>), String> {
        let mut storage = self.local_storage.lock().await;
        
        let group = storage.groups.entry(group_id.to_string()).or_insert_with(|| {
            GroupInfo {
                generation_id: 1,
                leader_id: member_id.to_string(),
                members: Vec::new(),
            }
        });
        
        if !group.members.iter().any(|m| m.member_id == member_id) {
            group.members.push(GroupMember {
                member_id: member_id.to_string(),
                metadata: Vec::new(),
            });
            group.generation_id += 1;
        }
        
        Ok((
            group.generation_id,
            group.leader_id.clone(),
            member_id.to_string(),
            group.members.clone(),
        ))
    }

    async fn sync_group(&mut self, _group_id: &str, _generation_id: i32, _member_id: &str) -> Result<Vec<u8>, String> {
        Ok(Vec::new())
    }

    async fn heartbeat(&mut self, group_id: &str, _generation_id: i32, member_id: &str) -> Result<(), String> {
        let storage = self.local_storage.lock().await;
        if let Some(group) = storage.groups.get(group_id) {
            if group.members.iter().any(|m| m.member_id == member_id) {
                return Ok(());
            }
        }
        Err("Unknown member".to_string())
    }

    async fn leave_group(&mut self, group_id: &str, member_id: &str) -> Result<(), String> {
        let mut storage = self.local_storage.lock().await;
        if let Some(group) = storage.groups.get_mut(group_id) {
            group.members.retain(|m| m.member_id != member_id);
            return Ok(());
        }
        Err("Group not found".to_string())
    }
}
