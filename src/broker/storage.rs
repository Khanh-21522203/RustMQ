use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::broker::config::RetentionConfig;

/// A single stored message with its offset and timestamp.
#[derive(Debug, Clone)]
pub struct StoredMessage {
    pub offset: i64,
    pub key: Option<Vec<u8>>,
    pub value: Vec<u8>,
    pub timestamp_ms: i64,
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
    async fn create_topic(&self, topic: &str, num_partitions: i32) -> Result<(), String>;
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
        protocol_metadata: &[u8],
        session_timeout_ms: i64,
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

#[derive(Clone, PartialEq)]
enum GroupStatus {
    Empty,
    Stable,
    PreparingRebalance,
}

struct GroupMemberState {
    pub member_id: String,
    pub metadata: Vec<u8>,
    pub last_heartbeat_ms: i64,
}

struct GroupState {
    generation_id: i32,
    leader_id: String,
    members: Vec<GroupMemberState>,
    subscriptions: HashMap<String, String>,  // member_id → topic
    assignments: HashMap<String, Vec<i32>>,  // member_id → partitions
    status: GroupStatus,
    rejoined: HashSet<String>,
    session_timeout_ms: i64,
    rebalance_started_ms: i64,
}

struct InnerStorage {
    // topic -> partition -> messages (index == offset)
    messages: HashMap<String, HashMap<i32, Vec<StoredMessage>>>,
    // topic -> num_partitions
    topics: HashMap<String, i32>,
    // group -> topic -> partition -> (offset, metadata)
    offsets: HashMap<String, HashMap<String, HashMap<i32, (i64, String)>>>,
    // group -> GroupState
    groups: HashMap<String, GroupState>,
}

impl InnerStorage {
    fn new() -> Self {
        Self {
            messages: HashMap::new(),
            topics: HashMap::new(),
            offsets: HashMap::new(),
            groups: HashMap::new(),
        }
    }
}

// ── InMemoryStorage ───────────────────────────────────────────────────────────

pub struct InMemoryStorage {
    inner: Arc<RwLock<InnerStorage>>,
    broker_id: i32,
    broker_host: String,
    broker_port: i32,
}

impl InMemoryStorage {
    pub fn new(broker_id: i32, broker_host: String, broker_port: i32) -> Self {
        let inner = Arc::new(RwLock::new(InnerStorage::new()));
        let storage = Self { inner: inner.clone(), broker_id, broker_host, broker_port };

        // Background task: expire dead members + finalize timed-out rebalances
        tokio::spawn(Self::background_task(inner));

        storage
    }

    pub fn new_with_retention(
        broker_id: i32,
        broker_host: String,
        broker_port: i32,
        retention: RetentionConfig,
    ) -> Self {
        let inner = Arc::new(RwLock::new(InnerStorage::new()));
        let inner_clone = inner.clone();

        // Retention task
        if retention.retention_ms.is_some() || retention.max_messages_per_partition.is_some() {
            let inner_ret = inner.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
                loop {
                    interval.tick().await;
                    let now = now_ms();
                    let mut store = inner_ret.write().await;
                    for partitions in store.messages.values_mut() {
                        for msgs in partitions.values_mut() {
                            if let Some(retain_ms) = retention.retention_ms {
                                msgs.retain(|m| now - m.timestamp_ms < retain_ms as i64);
                            }
                            if let Some(max) = retention.max_messages_per_partition {
                                if msgs.len() > max {
                                    let drop = msgs.len() - max;
                                    msgs.drain(0..drop);
                                }
                            }
                        }
                    }
                }
            });
        }

        // Heartbeat expiry + rebalance finalization task
        tokio::spawn(Self::background_task(inner_clone));

        Self { inner, broker_id, broker_host, broker_port }
    }

    async fn background_task(inner: Arc<RwLock<InnerStorage>>) {
        const REBALANCE_TIMEOUT_MS: i64 = 30_000;
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            interval.tick().await;
            let now = now_ms();
            let mut store = inner.write().await;
            let group_ids: Vec<String> = store.groups.keys().cloned().collect();
            for group_id in group_ids {
                let group = store.groups.get_mut(&group_id).unwrap();

                // Expire timed-out members
                let session_timeout = group.session_timeout_ms;
                let expired: Vec<String> = group.members.iter()
                    .filter(|m| now - m.last_heartbeat_ms > session_timeout)
                    .map(|m| m.member_id.clone())
                    .collect();
                for mid in &expired {
                    group.members.retain(|m| &m.member_id != mid);
                    group.subscriptions.remove(mid);
                    group.assignments.remove(mid);
                    group.rejoined.remove(mid);
                }
                if !expired.is_empty() {
                    if group.members.is_empty() {
                        group.status = GroupStatus::Empty;
                    } else {
                        in_memory_trigger_rebalance(group, &expired[0]);
                    }
                }

                // Finalize timed-out rebalance
                if group.status == GroupStatus::PreparingRebalance
                    && group.rebalance_started_ms > 0
                    && now - group.rebalance_started_ms > REBALANCE_TIMEOUT_MS
                {
                    let topics = store.topics.clone();
                    let group = store.groups.get_mut(&group_id).unwrap();
                    in_memory_finalize_rebalance(group, &topics);
                }
            }
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn in_memory_trigger_rebalance(group: &mut GroupState, joining_member_id: &str) {
    group.status = GroupStatus::PreparingRebalance;
    group.rejoined = HashSet::from([joining_member_id.to_string()]);
    group.generation_id += 1;
    group.rebalance_started_ms = now_ms();
}

fn in_memory_finalize_rebalance(group: &mut GroupState, topics: &HashMap<String, i32>) {
    let rejoined = group.rejoined.clone();
    group.members.retain(|m| rejoined.contains(&m.member_id));
    group.subscriptions.retain(|mid, _| rejoined.contains(mid));
    if group.members.is_empty() {
        group.status = GroupStatus::Empty;
    } else {
        group.assignments = compute_assignments(&group.subscriptions, topics);
        group.status = GroupStatus::Stable;
    }
    group.rejoined.clear();
    group.rebalance_started_ms = 0;
}

/// Round-robin partition assignment using explicit topic partition counts.
fn compute_assignments(
    subscriptions: &HashMap<String, String>,
    topics: &HashMap<String, i32>,
) -> HashMap<String, Vec<i32>> {
    let mut assignments: HashMap<String, Vec<i32>> = HashMap::new();
    let mut topic_members: HashMap<String, Vec<String>> = HashMap::new();
    for (member_id, topic) in subscriptions {
        topic_members.entry(topic.clone()).or_default().push(member_id.clone());
    }
    for (topic, mut members) in topic_members {
        members.sort();
        let num_parts = topics.get(&topic).copied().unwrap_or(1);
        let partitions: Vec<i32> = (0..num_parts).collect();
        for (i, member_id) in members.iter().enumerate() {
            let assigned: Vec<i32> = partitions.iter().enumerate()
                .filter(|(pi, _)| pi % members.len() == i)
                .map(|(_, &p)| p)
                .collect();
            assignments.insert(member_id.clone(), assigned);
        }
    }
    assignments
}

#[async_trait]
impl BrokerStorage for InMemoryStorage {
    async fn create_topic(&self, topic: &str, num_partitions: i32) -> Result<(), String> {
        if num_partitions <= 0 {
            return Err("num_partitions must be > 0".to_string());
        }
        let mut inner = self.inner.write().await;
        inner.topics.insert(topic.to_string(), num_partitions);
        let topic_data = inner.messages.entry(topic.to_string()).or_default();
        for p in 0..num_partitions {
            topic_data.entry(p).or_default();
        }
        Ok(())
    }

    async fn get_topics(&self) -> Vec<String> {
        self.inner.read().await.messages.keys().cloned().collect()
    }

    async fn get_topic_partitions(&self, topic: &str) -> Option<Vec<i32>> {
        self.inner.read().await.messages.get(topic)
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
        let partition_data = inner.messages
            .entry(topic.to_string()).or_default()
            .entry(partition).or_default();
        let offset = partition_data.len() as i64;
        partition_data.push(StoredMessage { offset, key, value, timestamp_ms: now_ms() });
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
        let Some(topic_data) = inner.messages.get(topic) else { return Ok(vec![]); };
        let Some(partition_data) = topic_data.get(&partition) else { return Ok(vec![]); };

        let start_idx = partition_data.partition_point(|m| m.offset < offset);
        if start_idx >= partition_data.len() { return Ok(vec![]); }

        let mut result = Vec::new();
        let mut total_bytes = 0usize;
        for msg in &partition_data[start_idx..] {
            let size = msg.value.len() + msg.key.as_ref().map_or(0, |k| k.len());
            if total_bytes + size > max_bytes as usize && !result.is_empty() { break; }
            result.push(msg.clone());
            total_bytes += size;
        }
        Ok(result)
    }

    async fn get_partition_offset(&self, topic: &str, partition: i32, time: i64) -> Result<Vec<i64>, String> {
        let inner = self.inner.read().await;
        if let Some(topic_data) = inner.messages.get(topic) {
            if let Some(partition_data) = topic_data.get(&partition) {
                let offset = match time {
                    -1 => vec![partition_data.len() as i64],
                    -2 => vec![partition_data.first().map_or(0, |m| m.offset)],
                    _ => vec![partition_data.len() as i64],
                };
                return Ok(offset);
            }
        }
        Ok(vec![0])
    }

    async fn commit_offset(&self, group: &str, topic: &str, partition: i32, offset: i64, metadata: String) -> Result<(), String> {
        let mut inner = self.inner.write().await;
        inner.offsets
            .entry(group.to_string()).or_default()
            .entry(topic.to_string()).or_default()
            .insert(partition, (offset, metadata));
        Ok(())
    }

    async fn fetch_offset(&self, group: &str, topic: &str, partition: i32) -> Result<(i64, String), String> {
        let inner = self.inner.read().await;
        Ok(inner.offsets.get(group)
            .and_then(|g| g.get(topic))
            .and_then(|t| t.get(&partition))
            .cloned()
            .unwrap_or((-1, String::new())))
    }

    async fn get_coordinator_info(&self) -> (i32, String, i32) {
        (self.broker_id, self.broker_host.clone(), self.broker_port)
    }

    async fn join_group(
        &self,
        group_id: &str,
        member_id: &str,
        _protocol_type: &str,
        protocol_metadata: &[u8],
        session_timeout_ms: i64,
    ) -> Result<(i32, String, String, Vec<GroupMember>), String> {
        let topic = std::str::from_utf8(protocol_metadata).unwrap_or("").to_string();
        let now = now_ms();
        let mut inner = self.inner.write().await;
        let group = inner.groups.entry(group_id.to_string()).or_insert_with(|| GroupState {
            generation_id: 0,
            leader_id: member_id.to_string(),
            members: Vec::new(),
            subscriptions: HashMap::new(),
            assignments: HashMap::new(),
            status: GroupStatus::Empty,
            rejoined: HashSet::new(),
            session_timeout_ms,
            rebalance_started_ms: 0,
        });

        let already_member = group.members.iter().any(|m| m.member_id == member_id);

        if already_member {
            // Update heartbeat
            if let Some(m) = group.members.iter_mut().find(|m| m.member_id == member_id) {
                m.last_heartbeat_ms = now;
            }
            if group.status == GroupStatus::PreparingRebalance {
                group.rejoined.insert(member_id.to_string());
                if group.rejoined.len() >= group.members.len() {
                    let topics = inner.topics.clone();
                    let group = inner.groups.get_mut(group_id).unwrap();
                    in_memory_finalize_rebalance(group, &topics);
                }
            }
        } else {
            // New member
            group.members.push(GroupMemberState {
                member_id: member_id.to_string(),
                metadata: protocol_metadata.to_vec(),
                last_heartbeat_ms: now,
            });
            if !topic.is_empty() {
                group.subscriptions.insert(member_id.to_string(), topic);
            }
            let has_existing = group.members.len() > 1;
            if !has_existing || group.status == GroupStatus::Empty {
                // First member — assign immediately
                let topics = inner.topics.clone();
                let group = inner.groups.get_mut(group_id).unwrap();
                group.assignments = compute_assignments(&group.subscriptions, &topics);
                group.status = GroupStatus::Stable;
            } else if group.status == GroupStatus::Stable {
                in_memory_trigger_rebalance(group, member_id);
            } else {
                // Already rebalancing — add to rejoined
                group.rejoined.insert(member_id.to_string());
                if group.rejoined.len() >= group.members.len() {
                    let topics = inner.topics.clone();
                    let group = inner.groups.get_mut(group_id).unwrap();
                    in_memory_finalize_rebalance(group, &topics);
                }
            }
        }

        let group = inner.groups.get(group_id).unwrap();
        let members: Vec<GroupMember> = group.members.iter()
            .map(|m| GroupMember { member_id: m.member_id.clone(), metadata: m.metadata.clone() })
            .collect();
        Ok((group.generation_id, group.leader_id.clone(), member_id.to_string(), members))
    }

    async fn sync_group(&self, group_id: &str, _generation_id: i32, member_id: &str) -> Result<Vec<u8>, String> {
        let inner = self.inner.read().await;
        let group = inner.groups.get(group_id).ok_or_else(|| "Group not found".to_string())?;
        if group.status == GroupStatus::PreparingRebalance {
            return Err("REBALANCE_IN_PROGRESS".to_string());
        }
        let assigned = group.assignments.get(member_id).cloned().unwrap_or_default();
        Ok(bincode::serialize(&assigned).unwrap_or_default())
    }

    async fn heartbeat(&self, group_id: &str, _generation_id: i32, member_id: &str) -> Result<(), String> {
        let mut inner = self.inner.write().await;
        let group = inner.groups.get_mut(group_id).ok_or_else(|| "Unknown member".to_string())?;
        let member = group.members.iter_mut().find(|m| m.member_id == member_id)
            .ok_or_else(|| "Unknown member".to_string())?;
        member.last_heartbeat_ms = now_ms();
        if group.status == GroupStatus::PreparingRebalance {
            return Err("REBALANCE_IN_PROGRESS".to_string());
        }
        Ok(())
    }

    async fn leave_group(&self, group_id: &str, member_id: &str) -> Result<(), String> {
        let mut inner = self.inner.write().await;
        let group = inner.groups.get_mut(group_id).ok_or_else(|| "Group not found".to_string())?;
        group.members.retain(|m| m.member_id != member_id);
        group.subscriptions.remove(member_id);
        group.assignments.remove(member_id);
        group.rejoined.remove(member_id);
        if group.members.is_empty() {
            group.status = GroupStatus::Empty;
        } else {
            let topics = inner.topics.clone();
            let group = inner.groups.get_mut(group_id).unwrap();
            in_memory_trigger_rebalance(group, member_id);
            // Note: rebalance started; existing members will see REBALANCE_IN_PROGRESS on heartbeat
            drop(topics);
        }
        Ok(())
    }
}
