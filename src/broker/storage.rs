use std::collections::HashMap;
use async_trait::async_trait;

/// Storage trait defining broker data operations
#[async_trait]
pub trait BrokerStorage: Send + Sync {
    async fn get_topics(&self) -> Vec<String>;
    async fn get_topic_partitions(&self, topic: &str) -> Option<Vec<i32>>;
    async fn produce_message(&mut self, topic: &str, partition: i32, message: Vec<u8>) -> Result<i64, String>;
    async fn fetch_messages(&self, topic: &str, partition: i32, offset: i64, max_bytes: i32) -> Result<(Vec<u8>, i64), String>;
    async fn get_partition_offset(&self, topic: &str, partition: i32, time: i64) -> Result<Vec<i64>, String>;
    async fn commit_offset(&mut self, group: &str, topic: &str, partition: i32, offset: i64, metadata: String) -> Result<(), String>;
    async fn fetch_offset(&self, group: &str, topic: &str, partition: i32) -> Result<(i64, String), String>;
    async fn get_coordinator_info(&self) -> (i32, String, i32);
    async fn join_group(&mut self, group_id: &str, member_id: &str, protocol_type: &str) -> Result<(i32, String, String, Vec<GroupMember>), String>;
    async fn sync_group(&mut self, group_id: &str, generation_id: i32, member_id: &str) -> Result<Vec<u8>, String>;
    async fn heartbeat(&mut self, group_id: &str, generation_id: i32, member_id: &str) -> Result<(), String>;
    async fn leave_group(&mut self, group_id: &str, member_id: &str) -> Result<(), String>;
}

#[derive(Clone)]
pub struct GroupMember {
    pub member_id: String,
    pub metadata: Vec<u8>,
}

/// In-memory storage implementation
pub struct InMemoryStorage {
    // topic -> partition -> messages [(offset, data)]
    messages: HashMap<String, HashMap<i32, Vec<(i64, Vec<u8>)>>>,
    // group -> topic -> partition -> (offset, metadata)
    offsets: HashMap<String, HashMap<String, HashMap<i32, (i64, String)>>>,
    // group -> GroupState
    groups: HashMap<String, GroupState>,
    broker_id: i32,
    broker_host: String,
    broker_port: i32,
}

struct GroupState {
    generation_id: i32,
    leader_id: String,
    protocol: String,
    members: Vec<GroupMember>,
}

impl InMemoryStorage {
    pub fn new(broker_id: i32, broker_host: String, broker_port: i32) -> Self {
        let mut messages = HashMap::new();
        
        // Initialize default topics for testing
        let mut topic_partitions = HashMap::new();
        topic_partitions.insert(0, Vec::new());
        topic_partitions.insert(1, Vec::new());
        messages.insert("test-topic".to_string(), topic_partitions);
        
        Self {
            messages,
            offsets: HashMap::new(),
            groups: HashMap::new(),
            broker_id,
            broker_host,
            broker_port,
        }
    }

    fn get_or_create_topic(&mut self, topic: &str) -> &mut HashMap<i32, Vec<(i64, Vec<u8>)>> {
        self.messages.entry(topic.to_string()).or_insert_with(|| {
            let mut partitions = HashMap::new();
            partitions.insert(0, Vec::new());
            partitions
        })
    }
}

#[async_trait]
impl BrokerStorage for InMemoryStorage {
    async fn get_topics(&self) -> Vec<String> {
        self.messages.keys().cloned().collect()
    }

    async fn get_topic_partitions(&self, topic: &str) -> Option<Vec<i32>> {
        self.messages.get(topic).map(|partitions| {
            partitions.keys().copied().collect()
        })
    }

    async fn produce_message(&mut self, topic: &str, partition: i32, message: Vec<u8>) -> Result<i64, String> {
        let topic_data = self.get_or_create_topic(topic);
        let partition_data = topic_data.entry(partition).or_insert_with(Vec::new);
        
        let offset = partition_data.len() as i64;
        partition_data.push((offset, message));
        Ok(offset)
    }

    async fn fetch_messages(&self, topic: &str, partition: i32, offset: i64, max_bytes: i32) -> Result<(Vec<u8>, i64), String> {
        if let Some(topic_data) = self.messages.get(topic) {
            if let Some(partition_data) = topic_data.get(&partition) {
                let mut result = Vec::new();
                let mut high_watermark = offset;
                
                for (msg_offset, data) in partition_data.iter() {
                    if *msg_offset >= offset && result.len() < max_bytes as usize {
                        result.extend_from_slice(data);
                        high_watermark = *msg_offset + 1;
                    }
                }
                
                high_watermark = high_watermark.max(partition_data.len() as i64);
                return Ok((result, high_watermark));
            }
        }
        Err("Topic or partition not found".to_string())
    }

    async fn get_partition_offset(&self, topic: &str, partition: i32, time: i64) -> Result<Vec<i64>, String> {
        if let Some(topic_data) = self.messages.get(topic) {
            if let Some(partition_data) = topic_data.get(&partition) {
                let offset = match time {
                    -1 => vec![partition_data.len() as i64], // Latest
                    -2 => vec![0],                            // Earliest
                    _ => vec![partition_data.len() as i64],   // Default to latest
                };
                return Ok(offset);
            }
        }
        Err("Topic or partition not found".to_string())
    }

    async fn commit_offset(&mut self, group: &str, topic: &str, partition: i32, offset: i64, metadata: String) -> Result<(), String> {
        let group_data = self.offsets.entry(group.to_string()).or_insert_with(HashMap::new);
        let topic_data = group_data.entry(topic.to_string()).or_insert_with(HashMap::new);
        topic_data.insert(partition, (offset, metadata));
        Ok(())
    }

    async fn fetch_offset(&self, group: &str, topic: &str, partition: i32) -> Result<(i64, String), String> {
        if let Some(group_data) = self.offsets.get(group) {
            if let Some(topic_data) = group_data.get(topic) {
                if let Some((offset, metadata)) = topic_data.get(&partition) {
                    return Ok((*offset, metadata.clone()));
                }
            }
        }
        // Return -1 to indicate no committed offset
        Ok((-1, String::new()))
    }

    async fn get_coordinator_info(&self) -> (i32, String, i32) {
        (self.broker_id, self.broker_host.clone(), self.broker_port)
    }

    async fn join_group(&mut self, group_id: &str, member_id: &str, protocol_type: &str) -> Result<(i32, String, String, Vec<GroupMember>), String> {
        let group = self.groups.entry(group_id.to_string()).or_insert_with(|| {
            GroupState {
                generation_id: 0,
                leader_id: member_id.to_string(),
                protocol: protocol_type.to_string(),
                members: Vec::new(),
            }
        });
        
        // Check if member already exists
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
        // Return empty assignment (in real implementation, this would contain partition assignments)
        Ok(Vec::new())
    }

    async fn heartbeat(&mut self, group_id: &str, _generation_id: i32, member_id: &str) -> Result<(), String> {
        if let Some(group) = self.groups.get(group_id) {
            if group.members.iter().any(|m| m.member_id == member_id) {
                return Ok(());
            }
        }
        Err("Unknown member".to_string())
    }

    async fn leave_group(&mut self, group_id: &str, member_id: &str) -> Result<(), String> {
        if let Some(group) = self.groups.get_mut(group_id) {
            group.members.retain(|m| m.member_id != member_id);
            return Ok(());
        }
        Err("Group not found".to_string())
    }
}
