use std::collections::HashMap;
use std::sync::Arc;
use async_trait::async_trait;
use tokio::sync::RwLock;

/// A single stored message with its offset.
#[derive(Debug, Clone)]
pub struct StoredMessage {
    pub offset: i64,
    pub key: Option<Vec<u8>>,
    pub value: Vec<u8>,
}

#[derive(Clone)]
pub struct GroupMember {
    pub member_id: String,
    pub metadata: Vec<u8>,
}

/// Storage trait defining broker data operations.
/// All methods take &self — implementations use interior mutability (Arc<RwLock<>>).
#[async_trait]
pub trait BrokerStorage: Send + Sync {
    async fn get_topics(&self) -> Vec<String>;
    async fn get_topic_partitions(&self, topic: &str) -> Option<Vec<i32>>;
    async fn produce_message(
        &self,
        topic: &str,
        partition: i32,
        key: Option<Vec<u8>>,
        value: Vec<u8>,
    ) -> Result<i64, String>;
    async fn fetch_messages(
        &self,
        topic: &str,
        partition: i32,
        offset: i64,
        max_bytes: i32,
    ) -> Result<Vec<StoredMessage>, String>;
    async fn get_partition_offset(
        &self,
        topic: &str,
        partition: i32,
        time: i64,
    ) -> Result<Vec<i64>, String>;
    async fn commit_offset(
        &self,
        group: &str,
        topic: &str,
        partition: i32,
        offset: i64,
        metadata: String,
    ) -> Result<(), String>;
    async fn fetch_offset(
        &self,
        group: &str,
        topic: &str,
        partition: i32,
    ) -> Result<(i64, String), String>;
    async fn get_coordinator_info(&self) -> (i32, String, i32);
    async fn join_group(
        &self,
        group_id: &str,
        member_id: &str,
        protocol_type: &str,
    ) -> Result<(i32, String, String, Vec<GroupMember>), String>;
    async fn sync_group(
        &self,
        group_id: &str,
        generation_id: i32,
        member_id: &str,
    ) -> Result<Vec<u8>, String>;
    async fn heartbeat(
        &self,
        group_id: &str,
        generation_id: i32,
        member_id: &str,
    ) -> Result<(), String>;
    async fn leave_group(&self, group_id: &str, member_id: &str) -> Result<(), String>;
}

// ── Internal state for InMemoryStorage ───────────────────────────────────────

struct InnerStorage {
    // topic -> partition -> messages (index == offset)
    messages: HashMap<String, HashMap<i32, Vec<StoredMessage>>>,
    // group -> topic -> partition -> (offset, metadata)
    offsets: HashMap<String, HashMap<String, HashMap<i32, (i64, String)>>>,
    // group -> GroupState
    groups: HashMap<String, GroupState>,
}

struct GroupState {
    generation_id: i32,
    leader_id: String,
    members: Vec<GroupMember>,
}

impl InnerStorage {
    fn new() -> Self {
        Self {
            messages: HashMap::new(),
            offsets: HashMap::new(),
            groups: HashMap::new(),
        }
    }
}

// ── InMemoryStorage ───────────────────────────────────────────────────────────

/// In-memory storage implementation.
pub struct InMemoryStorage {
    inner: Arc<RwLock<InnerStorage>>,
    broker_id: i32,
    broker_host: String,
    broker_port: i32,
}

impl InMemoryStorage {
    pub fn new(broker_id: i32, broker_host: String, broker_port: i32) -> Self {
        Self {
            inner: Arc::new(RwLock::new(InnerStorage::new())),
            broker_id,
            broker_host,
            broker_port,
        }
    }
}

#[async_trait]
impl BrokerStorage for InMemoryStorage {
    async fn get_topics(&self) -> Vec<String> {
        self.inner.read().await.messages.keys().cloned().collect()
    }

    async fn get_topic_partitions(&self, topic: &str) -> Option<Vec<i32>> {
        self.inner
            .read()
            .await
            .messages
            .get(topic)
            .map(|partitions| partitions.keys().copied().collect())
    }

    async fn produce_message(
        &self,
        topic: &str,
        partition: i32,
        key: Option<Vec<u8>>,
        value: Vec<u8>,
    ) -> Result<i64, String> {
        let mut inner = self.inner.write().await;
        let topic_data = inner
            .messages
            .entry(topic.to_string())
            .or_insert_with(HashMap::new);
        let partition_data = topic_data.entry(partition).or_insert_with(Vec::new);
        let offset = partition_data.len() as i64;
        partition_data.push(StoredMessage { offset, key, value });
        Ok(offset)
    }

    async fn fetch_messages(
        &self,
        topic: &str,
        partition: i32,
        offset: i64,
        max_bytes: i32,
    ) -> Result<Vec<StoredMessage>, String> {
        let inner = self.inner.read().await;
        let Some(topic_data) = inner.messages.get(topic) else {
            return Ok(vec![]);
        };
        let Some(partition_data) = topic_data.get(&partition) else {
            return Ok(vec![]);
        };

        if offset < 0 || offset >= partition_data.len() as i64 {
            return Ok(vec![]);
        }

        let mut result = Vec::new();
        let mut total_bytes = 0usize;
        for msg in &partition_data[offset as usize..] {
            let size = msg.value.len() + msg.key.as_ref().map_or(0, |k| k.len());
            if total_bytes + size > max_bytes as usize && !result.is_empty() {
                break;
            }
            result.push(msg.clone());
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
        let inner = self.inner.read().await;
        if let Some(topic_data) = inner.messages.get(topic) {
            if let Some(partition_data) = topic_data.get(&partition) {
                let offset = match time {
                    -1 => vec![partition_data.len() as i64],
                    -2 => vec![0],
                    _ => vec![partition_data.len() as i64],
                };
                return Ok(offset);
            }
        }
        // Unknown topic/partition: return earliest offset 0
        Ok(vec![0])
    }

    async fn commit_offset(
        &self,
        group: &str,
        topic: &str,
        partition: i32,
        offset: i64,
        metadata: String,
    ) -> Result<(), String> {
        let mut inner = self.inner.write().await;
        inner
            .offsets
            .entry(group.to_string())
            .or_insert_with(HashMap::new)
            .entry(topic.to_string())
            .or_insert_with(HashMap::new)
            .insert(partition, (offset, metadata));
        Ok(())
    }

    async fn fetch_offset(
        &self,
        group: &str,
        topic: &str,
        partition: i32,
    ) -> Result<(i64, String), String> {
        let inner = self.inner.read().await;
        if let Some(group_data) = inner.offsets.get(group) {
            if let Some(topic_data) = group_data.get(topic) {
                if let Some((offset, metadata)) = topic_data.get(&partition) {
                    return Ok((*offset, metadata.clone()));
                }
            }
        }
        // No committed offset
        Ok((-1, String::new()))
    }

    async fn get_coordinator_info(&self) -> (i32, String, i32) {
        (self.broker_id, self.broker_host.clone(), self.broker_port)
    }

    async fn join_group(
        &self,
        group_id: &str,
        member_id: &str,
        protocol_type: &str,
    ) -> Result<(i32, String, String, Vec<GroupMember>), String> {
        let _ = protocol_type;
        let mut inner = self.inner.write().await;
        let group = inner.groups.entry(group_id.to_string()).or_insert_with(|| GroupState {
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
        let inner = self.inner.read().await;
        if let Some(group) = inner.groups.get(group_id) {
            if group.members.iter().any(|m| m.member_id == member_id) {
                return Ok(());
            }
        }
        Err("Unknown member".to_string())
    }

    async fn leave_group(&self, group_id: &str, member_id: &str) -> Result<(), String> {
        let mut inner = self.inner.write().await;
        if let Some(group) = inner.groups.get_mut(group_id) {
            group.members.retain(|m| m.member_id != member_id);
            return Ok(());
        }
        Err("Group not found".to_string())
    }
}
