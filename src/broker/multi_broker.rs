use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::broker::simple_raft::{SimpleRaftNode, SimpleRaftStorage};
use crate::broker::storage::{BrokerStorage, GroupMember, StoredMessage};

struct GroupState {
    generation_id: i32,
    leader_id: String,
    members: Vec<GroupMember>,
}

struct LocalState {
    groups: HashMap<String, GroupState>,
}

/// Multi-broker implementation using Raft consensus.
pub struct MultiBroker {
    node_id: u64,
    raft_storage: SimpleRaftStorage,
    local: Arc<RwLock<LocalState>>,
    broker_host: String,
    broker_port: i32,
}

impl MultiBroker {
    pub fn new(node_id: u64, peers: Vec<u64>) -> Result<Self, String> {
        let (raft_node, raft_storage) = SimpleRaftNode::new(node_id, peers)
            .map_err(|e| format!("Failed to create raft node: {:?}", e))?;

        tokio::spawn(raft_node.run());

        Ok(Self {
            node_id,
            raft_storage,
            local: Arc::new(RwLock::new(LocalState { groups: HashMap::new() })),
            broker_host: "127.0.0.1".to_string(),
            broker_port: 9092 + node_id as i32,
        })
    }
}

#[async_trait]
impl BrokerStorage for MultiBroker {
    async fn get_topics(&self) -> Vec<String> {
        let data = self.raft_storage.read_data().await;
        data.messages
            .keys()
            .map(|(topic, _)| topic.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect()
    }

    async fn get_topic_partitions(&self, topic: &str) -> Option<Vec<i32>> {
        let data = self.raft_storage.read_data().await;
        let partitions: Vec<i32> = data
            .messages
            .keys()
            .filter(|(t, _)| t == topic)
            .map(|(_, p)| *p)
            .collect();
        if partitions.is_empty() {
            None
        } else {
            Some(partitions)
        }
    }

    async fn produce_message(
        &self,
        topic: &str,
        partition: i32,
        key: Option<Vec<u8>>,
        value: Vec<u8>,
    ) -> Result<i64, String> {
        self.raft_storage
            .propose_produce(topic.to_string(), partition, key, value)
            .await
            .map_err(|e| e.to_string())
    }

    async fn fetch_messages(
        &self,
        topic: &str,
        partition: i32,
        offset: i64,
        max_bytes: i32,
    ) -> Result<Vec<StoredMessage>, String> {
        let data = self.raft_storage.read_data().await;
        let Some(log) = data.messages.get(&(topic.to_owned(), partition)) else {
            return Ok(vec![]);
        };
        if offset < 0 || offset >= log.len() as i64 {
            return Ok(vec![]);
        }
        let mut result = Vec::new();
        let mut total_bytes = 0usize;
        for msg in &log[offset as usize..] {
            let size = msg.value.len() + msg.key.as_ref().map_or(0, |k| k.len());
            if total_bytes + size > max_bytes as usize && !result.is_empty() {
                break;
            }
            result.push(StoredMessage {
                offset: msg.offset,
                key: msg.key.clone(),
                value: msg.value.clone(),
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
    ) -> Result<Vec<i64>, String> {
        let data = self.raft_storage.read_data().await;
        let len = data
            .messages
            .get(&(topic.to_owned(), partition))
            .map_or(0, |log| log.len() as i64);
        let offset = match time {
            -1 => vec![len],
            -2 => vec![0],
            _ => vec![len],
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
    ) -> Result<(), String> {
        self.raft_storage
            .propose_commit_offset(group.to_string(), topic.to_string(), partition, offset)
            .await
            .map_err(|e| e.to_string())
    }

    async fn fetch_offset(
        &self,
        group: &str,
        topic: &str,
        partition: i32,
    ) -> Result<(i64, String), String> {
        let data = self.raft_storage.read_data().await;
        let offset = data
            .offsets
            .get(&(group.to_owned(), topic.to_owned(), partition))
            .copied()
            .unwrap_or(-1);
        Ok((offset, String::new()))
    }

    async fn get_coordinator_info(&self) -> (i32, String, i32) {
        (self.node_id as i32, self.broker_host.clone(), self.broker_port)
    }

    async fn join_group(
        &self,
        group_id: &str,
        member_id: &str,
        _protocol_type: &str,
    ) -> Result<(i32, String, String, Vec<GroupMember>), String> {
        let mut local = self.local.write().await;
        let group = local.groups.entry(group_id.to_string()).or_insert_with(|| GroupState {
            generation_id: 0,
            leader_id: member_id.to_string(),
            members: Vec::new(),
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

    async fn sync_group(
        &self,
        _group_id: &str,
        _generation_id: i32,
        _member_id: &str,
    ) -> Result<Vec<u8>, String> {
        Ok(Vec::new())
    }

    async fn heartbeat(
        &self,
        group_id: &str,
        _generation_id: i32,
        member_id: &str,
    ) -> Result<(), String> {
        let local = self.local.read().await;
        if let Some(group) = local.groups.get(group_id) {
            if group.members.iter().any(|m| m.member_id == member_id) {
                return Ok(());
            }
        }
        Err("Unknown member".to_string())
    }

    async fn leave_group(&self, group_id: &str, member_id: &str) -> Result<(), String> {
        let mut local = self.local.write().await;
        if let Some(group) = local.groups.get_mut(group_id) {
            group.members.retain(|m| m.member_id != member_id);
            return Ok(());
        }
        Err("Group not found".to_string())
    }
}
