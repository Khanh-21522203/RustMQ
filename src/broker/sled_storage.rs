use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;

use crate::broker::config::{RetentionConfig, DEFAULT_REBALANCE_TIMEOUT_MS};
use crate::broker::error::{BrokerError, BrokerResult};
use crate::broker::raft::{load_broker_data_from_sled, BrokerCommand, BrokerData, GroupStatus};
use crate::broker::state_machine::apply_raft_command;
use crate::broker::storage::{validate_topic_name, BrokerStorage, GroupMember, StoredMessage};

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

struct SledLocalState {
    heartbeats: HashMap<(String, String), i64>,
}

/// A single-node, sled-backed implementation of `BrokerStorage`.
///
/// All mutations are applied to an in-memory `BrokerData` view (for fast
/// reads) AND persisted to a sled database for durability.  On restart,
/// the in-memory view is reconstructed from sled.
pub struct SledStorage {
    db: Arc<sled::Db>,
    data: Arc<RwLock<BrokerData>>,
    local: Arc<RwLock<SledLocalState>>,
    broker_id: i32,
    broker_host: String,
    broker_port: i32,
}

impl SledStorage {
    pub fn open(
        broker_id: i32,
        broker_host: String,
        broker_port: i32,
        path: &str,
    ) -> anyhow::Result<Self> {
        Self::open_with_retention(broker_id, broker_host, broker_port, path, RetentionConfig::default())
    }

    pub fn open_with_retention(
        broker_id: i32,
        broker_host: String,
        broker_port: i32,
        path: &str,
        retention: RetentionConfig,
    ) -> anyhow::Result<Self> {
        let db = sled::open(path)?;
        let db = Arc::new(db);

        let initial_data = load_broker_data_from_sled(&db);
        let data = Arc::new(RwLock::new(initial_data));
        let local = Arc::new(RwLock::new(SledLocalState {
            heartbeats: HashMap::new(),
        }));

        // Background heartbeat expiry + rebalance finalization
        tokio::spawn(Self::background_task(
            db.clone(),
            data.clone(),
            local.clone(),
            DEFAULT_REBALANCE_TIMEOUT_MS,
        ));

        // Optional retention task
        if retention.retention_ms.is_some() || retention.max_messages_per_partition.is_some() {
            tokio::spawn(Self::retention_task(db.clone(), data.clone(), retention));
        }

        Ok(Self { db, data, local, broker_id, broker_host, broker_port })
    }

    async fn background_task(
        db: Arc<sled::Db>,
        data: Arc<RwLock<BrokerData>>,
        local: Arc<RwLock<SledLocalState>>,
        rebalance_timeout_ms: i64,
    ) {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            interval.tick().await;
            let now = now_ms();

            let (to_expire, to_finalize) = {
                let d = data.read().await;
                let heartbeats = local.read().await;
                let mut to_expire: Vec<(String, Vec<String>)> = Vec::new();
                let mut to_finalize: Vec<String> = Vec::new();

                for (group_id, group) in &d.groups {
                    let expired: Vec<String> = group
                        .members
                        .iter()
                        .filter(|m| {
                            let last = heartbeats
                                .heartbeats
                                .get(&(group_id.clone(), m.member_id.clone()))
                                .copied()
                                .unwrap_or(0);
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
                (to_expire, to_finalize)
            };

            for (group_id, expired_ids) in to_expire {
                let mut d = data.write().await;
                apply_raft_command(
                    &mut d,
                    &Some(db.clone()),
                    BrokerCommand::GroupExpire { group_id, expired_ids },
                );
            }
            for group_id in to_finalize {
                let mut d = data.write().await;
                apply_raft_command(
                    &mut d,
                    &Some(db.clone()),
                    BrokerCommand::GroupFinalize { group_id },
                );
            }
        }
    }

    async fn retention_task(
        db: Arc<sled::Db>,
        data: Arc<RwLock<BrokerData>>,
        retention: RetentionConfig,
    ) {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            interval.tick().await;
            let now = now_ms();

            // Compute truncation offsets under read lock
            let to_truncate: Vec<(String, i32, i64)> = {
                let d = data.read().await;
                let mut result = Vec::new();
                for ((topic, partition), log) in &d.messages {
                    let mut before_offset: Option<i64> = None;
                    if let Some(max) = retention.max_messages_per_partition {
                        if log.len() > max {
                            let drop = log.len() - max;
                            let candidate = if drop < log.len() { log[drop].offset } else { log.last().map_or(0, |m| m.offset + 1) };
                            before_offset = Some(candidate);
                        }
                    }
                    if let Some(ms) = retention.retention_ms {
                        let cutoff = now - ms as i64;
                        let drop = log.partition_point(|m| m.timestamp_ms < cutoff);
                        if drop > 0 {
                            let candidate = if drop < log.len() { log[drop].offset } else { log.last().map_or(0, |m| m.offset + 1) };
                            before_offset = Some(before_offset.map_or(candidate, |b| b.max(candidate)));
                        }
                    }
                    if let Some(off) = before_offset {
                        result.push((topic.clone(), *partition, off));
                    }
                }
                result
            };

            for (topic, partition, before_offset) in to_truncate {
                let mut d = data.write().await;
                apply_raft_command(
                    &mut d,
                    &Some(db.clone()),
                    BrokerCommand::TruncatePartition { topic, partition, before_offset },
                );
            }
        }
    }

    async fn apply(&self, cmd: BrokerCommand) -> i64 {
        let mut d = self.data.write().await;
        apply_raft_command(&mut d, &Some(self.db.clone()), cmd)
    }
}

#[async_trait]
impl BrokerStorage for SledStorage {
    async fn create_topic(&self, topic: &str, num_partitions: i32) -> BrokerResult<()> {
        validate_topic_name(topic)?;
        if num_partitions <= 0 {
            return Err(BrokerError::Validation("num_partitions must be > 0".to_string()));
        }
        self.apply(BrokerCommand::CreateTopic {
            topic: topic.to_string(),
            num_partitions,
        })
        .await;
        Ok(())
    }

    async fn get_topics(&self) -> Vec<String> {
        self.data.read().await.topics.keys().cloned().collect()
    }

    async fn get_topic_partitions(&self, topic: &str) -> Option<Vec<i32>> {
        self.data.read().await.topics.get(topic).map(|&n| (0..n).collect())
    }

    async fn produce_message(
        &self,
        topic: &str,
        partition: i32,
        key: Option<Vec<u8>>,
        value: Vec<u8>,
    ) -> BrokerResult<i64> {
        validate_topic_name(topic)?;
        let offset = self
            .apply(BrokerCommand::Produce {
                topic: topic.to_string(),
                partition,
                key,
                value,
            })
            .await;
        Ok(offset)
    }

    async fn fetch_messages(
        &self,
        topic: &str,
        partition: i32,
        offset: i64,
        max_bytes: i32,
    ) -> BrokerResult<Vec<StoredMessage>> {
        let data = self.data.read().await;
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
        let data = self.data.read().await;
        let log = data.messages.get(&(topic.to_owned(), partition));
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
                vec![log.and_then(|l| l.get(idx)).map_or(next_offset, |m| m.offset)]
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
        self.apply(BrokerCommand::CommitOffset {
            group_id: group.to_string(),
            topic: topic.to_string(),
            partition,
            offset,
        })
        .await;
        Ok(())
    }

    async fn fetch_offset(
        &self,
        group: &str,
        topic: &str,
        partition: i32,
    ) -> BrokerResult<(i64, String)> {
        let data = self.data.read().await;
        let offset = data
            .offsets
            .get(&(group.to_owned(), topic.to_owned(), partition))
            .copied()
            .unwrap_or(-1);
        Ok((offset, String::new()))
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
    ) -> BrokerResult<(i32, String, String, Vec<GroupMember>)> {
        let topic = std::str::from_utf8(protocol_metadata).unwrap_or("").to_string();

        // Record heartbeat on join
        {
            let mut local = self.local.write().await;
            local
                .heartbeats
                .insert((group_id.to_string(), member_id.to_string()), now_ms());
        }

        self.apply(BrokerCommand::GroupJoin {
            group_id: group_id.to_string(),
            member_id: member_id.to_string(),
            topic,
            metadata: protocol_metadata.to_vec(),
            session_timeout_ms,
        })
        .await;

        let data = self.data.read().await;
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
        let data = self.data.read().await;
        let group = data
            .groups
            .get(group_id)
            .ok_or_else(|| BrokerError::NotFound("Group not found".to_string()))?;
        if group.status == GroupStatus::PreparingRebalance {
            return Err(BrokerError::RebalanceInProgress);
        }
        let assigned = group.assignments.get(member_id).cloned().unwrap_or_default();
        Ok(bincode::serialize(&assigned).unwrap_or_default())
    }

    async fn heartbeat(
        &self,
        group_id: &str,
        _generation_id: i32,
        member_id: &str,
    ) -> BrokerResult<()> {
        {
            let mut local = self.local.write().await;
            local
                .heartbeats
                .insert((group_id.to_string(), member_id.to_string()), now_ms());
        }
        let data = self.data.read().await;
        let group = data.groups.get(group_id).ok_or_else(|| {
            BrokerError::NotFound(format!("Unknown group: {}", group_id))
        })?;
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

    async fn leave_group(&self, group_id: &str, member_id: &str) -> BrokerResult<()> {
        {
            let mut local = self.local.write().await;
            local
                .heartbeats
                .remove(&(group_id.to_string(), member_id.to_string()));
        }
        self.apply(BrokerCommand::GroupLeave {
            group_id: group_id.to_string(),
            member_id: member_id.to_string(),
        })
        .await;
        Ok(())
    }
}
