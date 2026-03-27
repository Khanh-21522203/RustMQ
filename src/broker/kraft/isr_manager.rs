use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

use crate::broker::kraft::partition_log::PartitionLog;

// ── Public types ──────────────────────────────────────────────────────────────

/// Notification emitted by [`IsrManager::tick`] when ISR membership or the
/// high-watermark changes on a partition this node is leading.
#[derive(Debug, Clone, PartialEq)]
pub struct IsrChange {
    pub topic: String,
    pub partition: i32,
    /// New in-sync replica set (sorted, includes the leader).
    pub new_isr: Vec<u64>,
    /// New high-watermark after advancing.
    pub new_hw: i64,
}

// ── Internal state ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct ReplicaProgress {
    /// The fetch offset this replica has acknowledged (= its current LEO).
    fetch_offset: i64,
    last_contact_ms: i64,
}

struct PartitionIsrState {
    /// All replica node IDs assigned to this partition (superset of ISR).
    all_replicas: Vec<u64>,
    /// Per-replica fetch progress, including the leader itself.
    progress: HashMap<u64, ReplicaProgress>,
    /// Current ISR snapshot (sorted).
    current_isr: Vec<u64>,
}

struct IsrManagerInner {
    /// Partitions this node is currently leading.
    partitions: HashMap<(String, i32), PartitionIsrState>,
    /// Partition logs — needed to read LEO and advance HW.
    logs: HashMap<(String, i32), Arc<PartitionLog>>,
}

// ── IsrManager ────────────────────────────────────────────────────────────────

/// Tracks follower fetch progress for partitions this node leads.
///
/// Responsibilities:
/// - Record fetch offsets reported by followers.
/// - On [`tick`], recompute the ISR (shrink/expand) and advance the partition
///   HW to `min(LEO across ISR members)`.
/// - Return [`IsrChange`] notifications so the caller can propose
///   `ControllerCommand::PartitionChange` when the ISR changes.
///
/// All methods are non-blocking (the inner lock is `tokio::sync::Mutex`).
pub struct IsrManager {
    inner: Arc<Mutex<IsrManagerInner>>,
    /// This broker's own node ID.  Its LEO counts as a progress entry.
    this_node: u64,
    /// A replica is removed from the ISR when it is more than `lag_max`
    /// messages behind the leader LEO.
    lag_max: i64,
    /// A replica is also removed when it hasn't contacted us for this long.
    stale_ms: i64,
}

impl IsrManager {
    /// Create a new `IsrManager`.
    ///
    /// * `this_node`  — this broker's node ID.
    /// * `lag_max`    — max message lag before a replica leaves the ISR.
    /// * `stale_ms`   — max milliseconds since last fetch before a replica is
    ///                  considered stale and removed from ISR.
    pub fn new(this_node: u64, lag_max: i64, stale_ms: u64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(IsrManagerInner {
                partitions: HashMap::new(),
                logs: HashMap::new(),
            })),
            this_node,
            lag_max,
            stale_ms: stale_ms as i64,
        }
    }

    /// Register a partition that this node has just become leader for.
    ///
    /// `replicas` is the full replica set (must include `this_node`).
    /// `log` is the local `PartitionLog` for this partition.
    pub async fn add_partition(
        &self,
        topic: String,
        partition: i32,
        replicas: Vec<u64>,
        initial_isr: Vec<u64>,
        log: Arc<PartitionLog>,
    ) {
        let key = (topic, partition);
        let now = now_ms();
        let leo = log.log_end_offset();

        let mut progress = HashMap::new();
        // Seed leader's own progress so it is always counted in ISR.
        progress.insert(
            self.this_node,
            ReplicaProgress {
                fetch_offset: leo,
                last_contact_ms: now,
            },
        );
        // Seed followers at offset 0; they will update on first fetch.
        for &rid in &replicas {
            if rid != self.this_node {
                progress.insert(
                    rid,
                    ReplicaProgress {
                        fetch_offset: 0,
                        last_contact_ms: 0, // unknown until first contact
                    },
                );
            }
        }

        let mut inner = self.inner.lock().await;
        inner.partitions.insert(
            key.clone(),
            PartitionIsrState {
                all_replicas: replicas,
                progress,
                current_isr: initial_isr,
            },
        );
        inner.logs.insert(key, log);
    }

    /// Deregister a partition (e.g. this node stepped down as leader).
    pub async fn remove_partition(&self, topic: &str, partition: i32) {
        let key = (topic.to_string(), partition);
        let mut inner = self.inner.lock().await;
        inner.partitions.remove(&key);
        inner.logs.remove(&key);
    }

    /// Record that `replica_id` has fetched up to `fetch_offset`.
    ///
    /// Called by the Fetch RPC handler when `replica_id` is non-zero (i.e. the
    /// caller is a follower replica, not a consumer).
    pub async fn record_fetch_progress(
        &self,
        topic: &str,
        partition: i32,
        replica_id: u64,
        fetch_offset: i64,
    ) {
        let key = (topic.to_string(), partition);
        let mut inner = self.inner.lock().await;
        if let Some(state) = inner.partitions.get_mut(&key) {
            state.progress.insert(
                replica_id,
                ReplicaProgress {
                    fetch_offset,
                    last_contact_ms: now_ms(),
                },
            );
        }
    }

    /// Recompute ISR membership and advance the HW for all tracked partitions.
    ///
    /// Returns a list of [`IsrChange`] for partitions whose ISR or HW changed.
    /// The caller should propose `ControllerCommand::PartitionChange` for each
    /// returned entry.
    pub async fn tick(&self) -> Vec<IsrChange> {
        let now = now_ms();
        let lag_max = self.lag_max;
        let stale_ms = self.stale_ms;
        let this_node = self.this_node;
        let mut changes = Vec::new();

        // Collect keys first to avoid simultaneous mutable + immutable borrows
        // on `inner.partitions` and `inner.logs`.
        let mut inner = self.inner.lock().await;
        let keys: Vec<(String, i32)> = inner.partitions.keys().cloned().collect();

        for key in keys {
            let log = match inner.logs.get(&key) {
                Some(l) => l.clone(),
                None => continue,
            };
            let leo = log.log_end_offset();

            let state = match inner.partitions.get_mut(&key) {
                Some(s) => s,
                None => continue,
            };

            // Keep leader's own progress current.
            state
                .progress
                .entry(this_node)
                .and_modify(|p| {
                    p.fetch_offset = leo;
                    p.last_contact_ms = now;
                })
                .or_insert(ReplicaProgress {
                    fetch_offset: leo,
                    last_contact_ms: now,
                });

            // Build new ISR: replicas within lag_max that have been contacted.
            let mut new_isr: Vec<u64> = state
                .all_replicas
                .iter()
                .copied()
                .filter(|&rid| {
                    if let Some(p) = state.progress.get(&rid) {
                        let in_lag = leo - p.fetch_offset <= lag_max;
                        let contacted = rid == this_node
                            || (p.last_contact_ms > 0
                                && now - p.last_contact_ms <= stale_ms);
                        in_lag && contacted
                    } else {
                        false
                    }
                })
                .collect();
            new_isr.sort();

            // HW = min fetch_offset across ISR members.
            let new_hw = new_isr
                .iter()
                .filter_map(|rid| state.progress.get(rid))
                .map(|p| p.fetch_offset)
                .min()
                .unwrap_or(0);

            let isr_changed = new_isr != state.current_isr;
            let hw_changed = new_hw > log.high_watermark();

            if isr_changed {
                state.current_isr = new_isr.clone();
            }
            if hw_changed {
                if let Err(e) = log.advance_hw(new_hw) {
                    log::error!("advance_hw failed for {}-{}: {e}", key.0, key.1);
                }
            }

            if isr_changed || hw_changed {
                let (topic, partition) = key;
                changes.push(IsrChange {
                    topic,
                    partition,
                    new_isr,
                    new_hw,
                });
            }
        }

        changes
    }
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

    fn temp_log() -> Arc<PartitionLog> {
        let db = sled::Config::new().temporary(true).open().unwrap();
        let tree = db.open_tree("p").unwrap();
        Arc::new(PartitionLog::open(tree).unwrap())
    }

    #[tokio::test]
    async fn single_node_hw_advances_with_leo() {
        let mgr = IsrManager::new(1, 1000, 30_000);
        let log = temp_log();

        // Append 3 messages; HW still 0 until tick runs.
        log.append(None, b"a".to_vec()).unwrap();
        log.append(None, b"b".to_vec()).unwrap();
        log.append(None, b"c".to_vec()).unwrap();

        mgr.add_partition("t".into(), 0, vec![1], vec![1], log.clone())
            .await;

        let changes = mgr.tick().await;
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].new_hw, 3);
        assert_eq!(changes[0].new_isr, vec![1]);
        assert_eq!(log.high_watermark(), 3);
    }

    #[tokio::test]
    async fn follower_progress_advances_hw() {
        let mgr = IsrManager::new(1, 1000, 30_000);
        let log = temp_log();
        log.append(None, b"x".to_vec()).unwrap();
        log.append(None, b"y".to_vec()).unwrap();

        mgr.add_partition("t".into(), 0, vec![1, 2], vec![1, 2], log.clone())
            .await;

        // Follower 2 has fetched offset 2 (caught up).
        mgr.record_fetch_progress("t", 0, 2, 2).await;

        let changes = mgr.tick().await;
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].new_hw, 2);
        assert_eq!(log.high_watermark(), 2);
    }

    #[tokio::test]
    async fn lagging_follower_leaves_isr() {
        // lag_max = 2, stale_ms = 60s
        let mgr = IsrManager::new(1, 2, 60_000);
        let log = temp_log();
        // Append 10 messages; follower only at offset 0 (10 behind)
        for _ in 0..10 {
            log.append(None, b"v".to_vec()).unwrap();
        }

        mgr.add_partition("t".into(), 0, vec![1, 2], vec![1, 2], log.clone())
            .await;
        // Record follower 2 at offset 0 (lag = 10 > lag_max 2)
        mgr.record_fetch_progress("t", 0, 2, 0).await;

        let changes = mgr.tick().await;
        assert_eq!(changes.len(), 1);
        // Follower 2 must have been removed from ISR
        assert_eq!(changes[0].new_isr, vec![1]);
    }

    #[tokio::test]
    async fn caught_up_follower_rejoins_isr() {
        let mgr = IsrManager::new(1, 2, 60_000);
        let log = temp_log();
        for _ in 0..5 {
            log.append(None, b"v".to_vec()).unwrap();
        }
        mgr.add_partition("t".into(), 0, vec![1, 2], vec![1], log.clone())
            .await;

        // Follower 2 catches up to LEO = 5
        mgr.record_fetch_progress("t", 0, 2, 5).await;

        let changes = mgr.tick().await;
        let change = changes.iter().find(|c| c.topic == "t").unwrap();
        assert!(change.new_isr.contains(&2), "follower 2 should rejoin ISR");
    }

    #[tokio::test]
    async fn hw_does_not_advance_beyond_slowest_isr_member() {
        let mgr = IsrManager::new(1, 100, 60_000);
        let log = temp_log();
        for _ in 0..10 {
            log.append(None, b"v".to_vec()).unwrap();
        }
        mgr.add_partition("t".into(), 0, vec![1, 2, 3], vec![1, 2, 3], log.clone())
            .await;

        // Follower 2 at 7, follower 3 at 4
        mgr.record_fetch_progress("t", 0, 2, 7).await;
        mgr.record_fetch_progress("t", 0, 3, 4).await;

        let changes = mgr.tick().await;
        let change = changes.iter().find(|c| c.topic == "t").unwrap();
        // HW = min(10, 7, 4) = 4
        assert_eq!(change.new_hw, 4);
        assert_eq!(log.high_watermark(), 4);
    }

    #[tokio::test]
    async fn no_change_emitted_when_isr_and_hw_are_stable() {
        let mgr = IsrManager::new(1, 1000, 60_000);
        let log = temp_log();
        log.append(None, b"a".to_vec()).unwrap();
        mgr.add_partition("t".into(), 0, vec![1], vec![1], log.clone())
            .await;

        // First tick advances HW → change emitted.
        let c1 = mgr.tick().await;
        assert_eq!(c1.len(), 1);

        // Second tick: nothing changed.
        let c2 = mgr.tick().await;
        assert!(c2.is_empty(), "no change expected on stable partition");
    }
}
