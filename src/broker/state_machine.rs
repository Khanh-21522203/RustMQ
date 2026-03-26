use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::broker::raft::{
    BrokerCommand, BrokerData, BrokerStoredMessage, GroupStatus, ReplicatedGroupMember,
    ReplicatedGroupState,
};

#[async_trait]
pub trait BrokerStateStore: Send + Sync {
    async fn load(&self) -> anyhow::Result<BrokerData>;
    async fn save(&self, data: &BrokerData) -> anyhow::Result<()>;
}

pub struct ReplicatedStateMachine<S: BrokerStateStore> {
    store: S,
    data: BrokerData,
}

impl<S: BrokerStateStore> ReplicatedStateMachine<S> {
    pub async fn open(store: S) -> anyhow::Result<Self> {
        let data = store.load().await?;
        Ok(Self { store, data })
    }

    pub async fn apply(&mut self, command: BrokerCommand) -> anyhow::Result<i64> {
        let result = apply_raft_command(&mut self.data, &None, command);
        self.store.save(&self.data).await?;
        Ok(result)
    }

    pub fn snapshot(&self) -> &BrokerData {
        &self.data
    }
}

pub fn apply_raft_command(
    data: &mut BrokerData,
    db: &Option<Arc<sled::Db>>,
    cmd: BrokerCommand,
) -> i64 {
    match cmd {
        BrokerCommand::Produce {
            topic,
            partition,
            key,
            value,
        } => {
            // Use high-watermark so offsets remain monotonic after retention truncation.
            let log_last = data
                .messages
                .get(&(topic.clone(), partition))
                .and_then(|l| l.last())
                .map(|m| m.offset + 1)
                .unwrap_or(0);
            let hwm = data
                .next_offsets
                .entry((topic.clone(), partition))
                .or_insert(0);
            let offset = (*hwm).max(log_last);
            *hwm = offset + 1;
            let log = data.messages.entry((topic.clone(), partition)).or_default();
            let ts = now_ms();
            log.push(BrokerStoredMessage {
                offset,
                key: key.clone(),
                value: value.clone(),
                timestamp_ms: ts,
            });
            if let Some(db) = db {
                let sled_key = format!("msg:{topic}:{partition:010}:{offset:020}");
                let msg = BrokerStoredMessage {
                    offset,
                    key,
                    value,
                    timestamp_ms: ts,
                };
                if let Ok(bytes) = bincode::serialize(&msg) {
                    let _ = db.insert(sled_key.as_bytes(), bytes);
                }
                let hwm_key = format!("hwm:{topic}:{partition:010}");
                if let Ok(bytes) = bincode::serialize(&(offset + 1)) {
                    let _ = db.insert(hwm_key.as_bytes(), bytes);
                }
            }
            offset
        }
        BrokerCommand::CommitOffset {
            group_id,
            topic,
            partition,
            offset,
        } => {
            data.offsets
                .insert((group_id.clone(), topic.clone(), partition), offset);
            if let Some(db) = db {
                let sled_key = format!("off:{group_id}:{topic}:{partition:010}");
                if let Ok(bytes) = bincode::serialize(&offset) {
                    let _ = db.insert(sled_key.as_bytes(), bytes);
                }
            }
            offset
        }
        BrokerCommand::CreateTopic {
            topic,
            num_partitions,
        } => {
            data.topics.insert(topic.clone(), num_partitions);
            for p in 0..num_partitions {
                data.messages.entry((topic.clone(), p)).or_default();
                data.next_offsets.entry((topic.clone(), p)).or_insert(0);
            }
            if let Some(db) = db {
                let sled_key = format!("top:{topic}");
                if let Ok(bytes) = bincode::serialize(&num_partitions) {
                    let _ = db.insert(sled_key.as_bytes(), bytes);
                }
            }
            0
        }
        BrokerCommand::TruncatePartition {
            topic,
            partition,
            before_offset,
        } => {
            if let Some(log) = data.messages.get_mut(&(topic.clone(), partition)) {
                log.retain(|m| m.offset >= before_offset);
                if let Some(db) = db {
                    let prefix = format!("msg:{topic}:{partition:010}:");
                    for key in db.scan_prefix(prefix.as_bytes()).keys().flatten() {
                        if let Ok(key_str) = std::str::from_utf8(&key) {
                            let parts: Vec<&str> = key_str.split(':').collect();
                            if let Some(off_str) = parts.last() {
                                if let Ok(off) = off_str.parse::<i64>() {
                                    if off < before_offset {
                                        let _ = db.remove(&key);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            0
        }
        BrokerCommand::GroupJoin {
            group_id,
            member_id,
            topic,
            metadata,
            session_timeout_ms,
        } => {
            let topics_snapshot = data.topics.clone();
            let group =
                data.groups
                    .entry(group_id.clone())
                    .or_insert_with(|| ReplicatedGroupState {
                        session_timeout_ms,
                        leader_id: member_id.clone(),
                        ..Default::default()
                    });

            let already_member = group.members.iter().any(|m| m.member_id == member_id);
            if already_member {
                if group.status == GroupStatus::PreparingRebalance {
                    group.rejoined.insert(member_id.clone());
                    if group.rejoined.len() >= group.members.len() {
                        finalize_rebalance(group, &topics_snapshot);
                    }
                }
            } else {
                group.members.push(ReplicatedGroupMember {
                    member_id: member_id.clone(),
                    metadata,
                });
                group.subscriptions.insert(member_id.clone(), topic);
                let has_existing = group.members.len() > 1;
                if has_existing && group.status == GroupStatus::Stable {
                    trigger_rebalance(group, &member_id);
                } else if group.status == GroupStatus::Empty || group.members.len() == 1 {
                    group.status = GroupStatus::Stable;
                    compute_and_store_assignments(group, &topics_snapshot);
                } else if group.status == GroupStatus::PreparingRebalance {
                    group.rejoined.insert(member_id.clone());
                    if group.rejoined.len() >= group.members.len() {
                        finalize_rebalance(group, &topics_snapshot);
                    }
                }
            }
            persist_group(db, &group_id, group);
            0
        }
        BrokerCommand::GroupLeave {
            group_id,
            member_id,
        } => {
            if let Some(group) = data.groups.get_mut(&group_id) {
                remove_member(group, &member_id);
                if group.members.is_empty() {
                    group.status = GroupStatus::Empty;
                } else {
                    trigger_rebalance(group, &member_id);
                }
                let group_clone = group.clone();
                persist_group(db, &group_id, &group_clone);
            }
            0
        }
        BrokerCommand::GroupExpire {
            group_id,
            expired_ids,
        } => {
            if let Some(group) = data.groups.get_mut(&group_id) {
                for id in &expired_ids {
                    remove_member(group, id);
                }
                if group.members.is_empty() {
                    group.status = GroupStatus::Empty;
                } else {
                    let first = expired_ids.first().cloned().unwrap_or_default();
                    trigger_rebalance(group, &first);
                }
                let group_clone = group.clone();
                persist_group(db, &group_id, &group_clone);
            }
            0
        }
        BrokerCommand::GroupFinalize { group_id } => {
            let topics_snapshot = data.topics.clone();
            if let Some(group) = data.groups.get_mut(&group_id) {
                if group.status == GroupStatus::PreparingRebalance {
                    finalize_rebalance(group, &topics_snapshot);
                }
                let group_clone = group.clone();
                persist_group(db, &group_id, &group_clone);
            }
            0
        }
    }
}

fn trigger_rebalance(group: &mut ReplicatedGroupState, joining_member_id: &str) {
    group.status = GroupStatus::PreparingRebalance;
    group.rejoined = HashSet::from([joining_member_id.to_string()]);
    group.generation_id += 1;
    group.rebalance_started_ms = now_ms();
}

fn finalize_rebalance(group: &mut ReplicatedGroupState, topics: &HashMap<String, i32>) {
    let rejoined = group.rejoined.clone();
    group.members.retain(|m| rejoined.contains(&m.member_id));
    group.subscriptions.retain(|mid, _| rejoined.contains(mid));
    if group.members.is_empty() {
        group.status = GroupStatus::Empty;
    } else {
        compute_and_store_assignments(group, topics);
        group.status = GroupStatus::Stable;
    }
    group.rejoined.clear();
    group.rebalance_started_ms = 0;
}

fn remove_member(group: &mut ReplicatedGroupState, member_id: &str) {
    group.members.retain(|m| m.member_id != member_id);
    group.subscriptions.remove(member_id);
    group.assignments.remove(member_id);
    group.rejoined.remove(member_id);
}

fn compute_and_store_assignments(group: &mut ReplicatedGroupState, topics: &HashMap<String, i32>) {
    let mut by_topic: HashMap<String, Vec<String>> = HashMap::new();
    for (mid, topic) in &group.subscriptions {
        by_topic.entry(topic.clone()).or_default().push(mid.clone());
    }
    group.assignments.clear();
    for (topic, mut members) in by_topic {
        members.sort();
        let num_parts = topics.get(&topic).copied().unwrap_or(1);
        let mut partitions: Vec<i32> = (0..num_parts).collect();
        partitions.sort();
        for (i, mid) in members.iter().enumerate() {
            let assigned: Vec<i32> = partitions
                .iter()
                .enumerate()
                .filter(|(pi, _)| pi % members.len() == i)
                .map(|(_, &p)| p)
                .collect();
            group.assignments.insert(mid.clone(), assigned);
        }
    }
}

fn persist_group(db: &Option<Arc<sled::Db>>, group_id: &str, group: &ReplicatedGroupState) {
    if let Some(db) = db {
        let sled_key = format!("grp:{group_id}");
        if let Ok(bytes) = bincode::serialize(group) {
            let _ = db.insert(sled_key.as_bytes(), bytes);
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
