use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;

use crate::broker::config::DEFAULT_REBALANCE_TIMEOUT_MS;
use crate::broker::error::{BrokerError, BrokerResult};
use crate::broker::storage::GroupMember;

// ── Internal state ────────────────────────────────────────────────────────────

#[derive(Clone, PartialEq)]
enum GroupStatus {
    Empty,
    Stable,
    PreparingRebalance,
}

struct GroupMemberState {
    member_id: String,
    metadata: Vec<u8>,
    last_heartbeat_ms: i64,
}

struct GroupState {
    generation_id: i32,
    leader_id: String,
    members: Vec<GroupMemberState>,
    /// member_id → subscribed topic
    subscriptions: HashMap<String, String>,
    /// member_id → assigned partition list
    assignments: HashMap<String, Vec<i32>>,
    status: GroupStatus,
    rejoined: HashSet<String>,
    session_timeout_ms: i64,
    rebalance_started_ms: i64,
}

struct CoordinatorInner {
    groups: HashMap<String, GroupState>,
    /// Topic partition counts, kept in sync by the owning broker.
    topics: HashMap<String, i32>,
}

// ── ConsumerGroupCoordinator ──────────────────────────────────────────────────

/// Local (non-replicated) consumer group coordinator.
///
/// Handles join/sync/heartbeat/leave and runs background member-expiry and
/// rebalance-finalization. Topic partition counts must be kept up-to-date
/// via [`update_topic`] / [`remove_topic`] so assignments are correct.
///
/// This type is `Clone + Send + Sync` through its inner `Arc`.
#[derive(Clone)]
pub struct ConsumerGroupCoordinator {
    inner: Arc<RwLock<CoordinatorInner>>,
    rebalance_timeout_ms: i64,
}

impl ConsumerGroupCoordinator {
    /// Create a new coordinator and start the background expiry task.
    pub fn new() -> Self {
        Self::with_rebalance_timeout(DEFAULT_REBALANCE_TIMEOUT_MS)
    }

    pub fn with_rebalance_timeout(rebalance_timeout_ms: i64) -> Self {
        let inner = Arc::new(RwLock::new(CoordinatorInner {
            groups: HashMap::new(),
            topics: HashMap::new(),
        }));
        let coordinator = Self {
            inner: inner.clone(),
            rebalance_timeout_ms,
        };
        tokio::spawn(Self::background_task(inner, rebalance_timeout_ms));
        coordinator
    }

    /// Notify the coordinator that a topic exists with the given partition count.
    /// Call this when a topic is created or its partition count changes.
    pub async fn update_topic(&self, topic: String, num_partitions: i32) {
        self.inner.write().await.topics.insert(topic, num_partitions);
    }

    /// Remove a topic from the coordinator's view (e.g. on topic deletion).
    pub async fn remove_topic(&self, topic: &str) {
        self.inner.write().await.topics.remove(topic);
    }

    // ── Protocol operations ───────────────────────────────────────────────────

    pub async fn join_group(
        &self,
        group_id: &str,
        member_id: &str,
        protocol_metadata: &[u8],
        session_timeout_ms: i64,
    ) -> BrokerResult<(i32, String, String, Vec<GroupMember>)> {
        let topic = std::str::from_utf8(protocol_metadata)
            .unwrap_or("")
            .to_string();
        let now = now_ms();
        let mut inner = self.inner.write().await;

        let group = inner
            .groups
            .entry(group_id.to_string())
            .or_insert_with(|| GroupState {
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
            if let Some(m) = group.members.iter_mut().find(|m| m.member_id == member_id) {
                m.last_heartbeat_ms = now;
            }
            if group.status == GroupStatus::PreparingRebalance {
                group.rejoined.insert(member_id.to_string());
                if group.rejoined.len() >= group.members.len() {
                    let topics = inner.topics.clone();
                    finalize_rebalance(inner.groups.get_mut(group_id).unwrap(), &topics);
                }
            }
        } else {
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
                let topics = inner.topics.clone();
                let group = inner.groups.get_mut(group_id).unwrap();
                group.assignments = compute_assignments(&group.subscriptions, &topics);
                group.status = GroupStatus::Stable;
            } else if group.status == GroupStatus::Stable {
                trigger_rebalance(group, member_id);
            } else {
                group.rejoined.insert(member_id.to_string());
                if group.rejoined.len() >= group.members.len() {
                    let topics = inner.topics.clone();
                    finalize_rebalance(inner.groups.get_mut(group_id).unwrap(), &topics);
                }
            }
        }

        let group = inner.groups.get(group_id).unwrap();
        let members = group
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

    pub async fn sync_group(
        &self,
        group_id: &str,
        member_id: &str,
    ) -> BrokerResult<Vec<u8>> {
        let inner = self.inner.read().await;
        let group = inner
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

    pub async fn heartbeat(
        &self,
        group_id: &str,
        member_id: &str,
    ) -> BrokerResult<()> {
        let mut inner = self.inner.write().await;
        let group = inner
            .groups
            .get_mut(group_id)
            .ok_or_else(|| BrokerError::NotFound(format!("Unknown group: {}", group_id)))?;
        let member = group
            .members
            .iter_mut()
            .find(|m| m.member_id == member_id)
            .ok_or_else(|| {
                BrokerError::NotFound(format!(
                    "Unknown member '{}' in group '{}'",
                    member_id, group_id
                ))
            })?;
        member.last_heartbeat_ms = now_ms();
        if group.status == GroupStatus::PreparingRebalance {
            return Err(BrokerError::RebalanceInProgress);
        }
        Ok(())
    }

    pub async fn leave_group(&self, group_id: &str, member_id: &str) -> BrokerResult<()> {
        let mut inner = self.inner.write().await;
        let group = inner
            .groups
            .get_mut(group_id)
            .ok_or_else(|| BrokerError::NotFound("Group not found".to_string()))?;
        group.members.retain(|m| m.member_id != member_id);
        group.subscriptions.remove(member_id);
        group.assignments.remove(member_id);
        group.rejoined.remove(member_id);
        if group.members.is_empty() {
            group.status = GroupStatus::Empty;
        } else {
            trigger_rebalance(group, member_id);
        }
        Ok(())
    }

    // ── Background task ───────────────────────────────────────────────────────

    async fn background_task(inner: Arc<RwLock<CoordinatorInner>>, rebalance_timeout_ms: i64) {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            interval.tick().await;
            let now = now_ms();
            let mut store = inner.write().await;
            let group_ids: Vec<String> = store.groups.keys().cloned().collect();

            for group_id in group_ids {
                // Expire timed-out members
                let session_timeout = match store.groups.get(&group_id) {
                    Some(g) => g.session_timeout_ms,
                    None => continue,
                };
                let expired: Vec<String> = store
                    .groups
                    .get(&group_id)
                    .map(|g| {
                        g.members
                            .iter()
                            .filter(|m| now - m.last_heartbeat_ms > session_timeout)
                            .map(|m| m.member_id.clone())
                            .collect()
                    })
                    .unwrap_or_default();

                if !expired.is_empty() {
                    if let Some(group) = store.groups.get_mut(&group_id) {
                        for mid in &expired {
                            group.members.retain(|m| &m.member_id != mid);
                            group.subscriptions.remove(mid);
                            group.assignments.remove(mid);
                            group.rejoined.remove(mid);
                        }
                        if group.members.is_empty() {
                            group.status = GroupStatus::Empty;
                        } else {
                            trigger_rebalance(group, &expired[0]);
                        }
                    }
                }

                // Finalize timed-out rebalance
                let should_finalize = store.groups.get(&group_id).map_or(false, |g| {
                    g.status == GroupStatus::PreparingRebalance
                        && g.rebalance_started_ms > 0
                        && now - g.rebalance_started_ms > rebalance_timeout_ms
                });
                if should_finalize {
                    let topics = store.topics.clone();
                    if let Some(group) = store.groups.get_mut(&group_id) {
                        finalize_rebalance(group, &topics);
                    }
                }
            }
        }
    }
}

impl Default for ConsumerGroupCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

// ── Pure helper functions ─────────────────────────────────────────────────────

fn trigger_rebalance(group: &mut GroupState, joining_member_id: &str) {
    group.status = GroupStatus::PreparingRebalance;
    group.rejoined = HashSet::from([joining_member_id.to_string()]);
    group.generation_id += 1;
    group.rebalance_started_ms = now_ms();
}

fn finalize_rebalance(group: &mut GroupState, topics: &HashMap<String, i32>) {
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

/// Round-robin partition assignment across subscribed members.
fn compute_assignments(
    subscriptions: &HashMap<String, String>,
    topics: &HashMap<String, i32>,
) -> HashMap<String, Vec<i32>> {
    let mut by_topic: HashMap<String, Vec<String>> = HashMap::new();
    for (member_id, topic) in subscriptions {
        by_topic
            .entry(topic.clone())
            .or_default()
            .push(member_id.clone());
    }
    let mut assignments: HashMap<String, Vec<i32>> = HashMap::new();
    for (topic, mut members) in by_topic {
        members.sort();
        let num_parts = topics.get(&topic).copied().unwrap_or(1);
        let partitions: Vec<i32> = (0..num_parts).collect();
        for (i, member_id) in members.iter().enumerate() {
            let assigned: Vec<i32> = partitions
                .iter()
                .enumerate()
                .filter(|(pi, _)| pi % members.len() == i)
                .map(|(_, &p)| p)
                .collect();
            assignments.insert(member_id.clone(), assigned);
        }
    }
    assignments
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    async fn coord_with_topic(topic: &str, num_partitions: i32) -> ConsumerGroupCoordinator {
        let c = ConsumerGroupCoordinator::new();
        c.update_topic(topic.to_string(), num_partitions).await;
        c
    }

    #[tokio::test]
    async fn first_member_gets_all_partitions() {
        let coord = coord_with_topic("events", 3).await;
        let (gen, leader, assigned_id, members) = coord
            .join_group("g1", "m1", b"events", 30_000)
            .await
            .unwrap();
        assert_eq!(gen, 0);
        assert_eq!(leader, "m1");
        assert_eq!(assigned_id, "m1");
        assert_eq!(members.len(), 1);

        let assignment: Vec<i32> =
            bincode::deserialize(&coord.sync_group("g1", "m1").await.unwrap()).unwrap();
        assert_eq!(assignment, vec![0, 1, 2]);
    }

    #[tokio::test]
    async fn second_member_triggers_rebalance() {
        let coord = coord_with_topic("events", 2).await;
        coord.join_group("g1", "m1", b"events", 30_000).await.unwrap();

        // m2 joins → rebalance starts
        let (gen, _, _, _) = coord
            .join_group("g1", "m2", b"events", 30_000)
            .await
            .unwrap();
        assert_eq!(gen, 1);

        // sync_group during rebalance → REBALANCE_IN_PROGRESS
        let err = coord.sync_group("g1", "m1").await.unwrap_err();
        assert!(matches!(err, BrokerError::RebalanceInProgress));

        // Both members rejoin → rebalance finalizes
        coord.join_group("g1", "m1", b"events", 30_000).await.unwrap();
        coord.join_group("g1", "m2", b"events", 30_000).await.unwrap();

        // Now sync should succeed and split partitions
        let a1: Vec<i32> =
            bincode::deserialize(&coord.sync_group("g1", "m1").await.unwrap()).unwrap();
        let a2: Vec<i32> =
            bincode::deserialize(&coord.sync_group("g1", "m2").await.unwrap()).unwrap();
        let mut combined = [a1, a2].concat();
        combined.sort();
        assert_eq!(combined, vec![0, 1]);
    }

    #[tokio::test]
    async fn heartbeat_unknown_member_returns_error() {
        let coord = ConsumerGroupCoordinator::new();
        coord.join_group("g1", "m1", b"t", 30_000).await.unwrap();
        let err = coord.heartbeat("g1", "nobody").await.unwrap_err();
        assert!(matches!(err, BrokerError::NotFound(_)));
    }

    #[tokio::test]
    async fn heartbeat_unknown_group_returns_error() {
        let coord = ConsumerGroupCoordinator::new();
        let err = coord.heartbeat("no-group", "m1").await.unwrap_err();
        assert!(matches!(err, BrokerError::NotFound(_)));
    }

    #[tokio::test]
    async fn leave_group_triggers_rebalance_for_remaining() {
        let coord = coord_with_topic("t", 2).await;
        coord.join_group("g1", "m1", b"t", 30_000).await.unwrap();
        coord.join_group("g1", "m2", b"t", 30_000).await.unwrap();
        // Rejoin both to stabilize
        coord.join_group("g1", "m1", b"t", 30_000).await.unwrap();
        coord.join_group("g1", "m2", b"t", 30_000).await.unwrap();

        // m2 leaves
        coord.leave_group("g1", "m2").await.unwrap();

        // m1 rejoins to complete rebalance and pick up all partitions
        coord.join_group("g1", "m1", b"t", 30_000).await.unwrap();
        let assignment: Vec<i32> =
            bincode::deserialize(&coord.sync_group("g1", "m1").await.unwrap()).unwrap();
        assert_eq!(assignment, vec![0, 1]);
    }

    #[tokio::test]
    async fn update_topic_affects_future_assignments() {
        let coord = ConsumerGroupCoordinator::new();
        coord.update_topic("t".to_string(), 1).await;
        coord.join_group("g1", "m1", b"t", 30_000).await.unwrap();
        let a: Vec<i32> =
            bincode::deserialize(&coord.sync_group("g1", "m1").await.unwrap()).unwrap();
        assert_eq!(a, vec![0]);

        // Topic grows to 3 partitions; trigger rebalance manually by having m1 rejoin
        coord.update_topic("t".to_string(), 3).await;
        coord.leave_group("g1", "m1").await.unwrap();
        coord.join_group("g1", "m1", b"t", 30_000).await.unwrap();
        let a: Vec<i32> =
            bincode::deserialize(&coord.sync_group("g1", "m1").await.unwrap()).unwrap();
        assert_eq!(a, vec![0, 1, 2]);
    }

    #[tokio::test]
    async fn round_robin_assignment_is_balanced() {
        let coord = coord_with_topic("t", 4).await;
        coord.join_group("g1", "m1", b"t", 30_000).await.unwrap();
        coord.join_group("g1", "m2", b"t", 30_000).await.unwrap();
        // Both rejoin to finalize
        coord.join_group("g1", "m1", b"t", 30_000).await.unwrap();
        coord.join_group("g1", "m2", b"t", 30_000).await.unwrap();

        let a1: Vec<i32> =
            bincode::deserialize(&coord.sync_group("g1", "m1").await.unwrap()).unwrap();
        let a2: Vec<i32> =
            bincode::deserialize(&coord.sync_group("g1", "m2").await.unwrap()).unwrap();
        assert_eq!(a1.len(), 2);
        assert_eq!(a2.len(), 2);
        let mut all = [a1, a2].concat();
        all.sort();
        assert_eq!(all, vec![0, 1, 2, 3]);
    }
}
