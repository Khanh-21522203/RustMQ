use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::broker::kraft::partition_log::PartitionLog;

// ── ReplicaFetcher trait ──────────────────────────────────────────────────────

/// A single fetched record returned by [`ReplicaFetcher::fetch`].
#[derive(Debug, Clone)]
pub struct FetchedRecord {
    pub key: Option<Vec<u8>>,
    pub value: Vec<u8>,
}

/// Fetch records from a partition leader.
///
/// The production implementation calls the leader's gRPC `Fetch` RPC.
/// Tests supply a mock that returns canned data.
#[async_trait]
pub trait ReplicaFetcher: Send + Sync + 'static {
    /// Fetch up to `max_bytes` of records starting at `fetch_offset`.
    /// Also returns the leader's current high-watermark so the follower can
    /// advance its own HW.
    async fn fetch(
        &self,
        topic: &str,
        partition: i32,
        fetch_offset: i64,
        max_bytes: i32,
    ) -> anyhow::Result<FetchResult>;
}

/// Result of a single fetch round-trip.
pub struct FetchResult {
    /// Records to append to the local log (in offset order).
    pub records: Vec<FetchedRecord>,
    /// Leader's current high-watermark.
    pub leader_hw: i64,
}

// ── ReplicationManager ────────────────────────────────────────────────────────

/// Manages per-partition follower fetch loops.
///
/// When this broker is a **follower** for a partition, `ReplicationManager`
/// runs a background task that continuously fetches from the leader and
/// appends to the local [`PartitionLog`].
///
/// When this broker becomes the **leader** (or the partition is removed), call
/// [`stop_fetch_task`] to cancel the task.
pub struct ReplicationManager {
    tasks: Arc<Mutex<HashMap<(String, i32), JoinHandle<()>>>>,
}

impl ReplicationManager {
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Start a fetch loop for `(topic, partition)` against the leader at
    /// `fetcher`.  If a task is already running for this partition it is
    /// stopped first.
    pub async fn start_fetch_task(
        &self,
        topic: String,
        partition: i32,
        log: Arc<PartitionLog>,
        fetcher: impl ReplicaFetcher,
    ) {
        let key = (topic.clone(), partition);

        // Cancel any existing task.
        let old = self.tasks.lock().await.remove(&key);
        if let Some(h) = old {
            h.abort();
        }

        let handle = tokio::spawn(fetch_loop(topic, partition, log, fetcher));
        self.tasks.lock().await.insert(key, handle);
    }

    /// Stop the fetch loop for `(topic, partition)`, if any.
    pub async fn stop_fetch_task(&self, topic: &str, partition: i32) {
        let key = (topic.to_string(), partition);
        if let Some(h) = self.tasks.lock().await.remove(&key) {
            h.abort();
        }
    }

    /// Restart the fetch loop for `(topic, partition)` against a new leader
    /// (e.g. after a controller-reported leader change).
    pub async fn update_leader(
        &self,
        topic: String,
        partition: i32,
        log: Arc<PartitionLog>,
        fetcher: impl ReplicaFetcher,
    ) {
        // start_fetch_task already handles replacing any existing task.
        self.start_fetch_task(topic, partition, log, fetcher).await;
    }

    /// Cancel all running fetch tasks (used on broker shutdown).
    pub async fn stop_all(&self) {
        let mut tasks = self.tasks.lock().await;
        for (_, h) in tasks.drain() {
            h.abort();
        }
    }

    /// Number of currently active fetch tasks.
    pub async fn active_task_count(&self) -> usize {
        self.tasks.lock().await.len()
    }
}

impl Default for ReplicationManager {
    fn default() -> Self {
        Self::new()
    }
}

// ── Fetch loop ────────────────────────────────────────────────────────────────

const MAX_FETCH_BYTES: i32 = 1024 * 1024; // 1 MiB per fetch
const POLL_INTERVAL_MS: u64 = 50;
const BACKOFF_MS: u64 = 500;

async fn fetch_loop(
    topic: String,
    partition: i32,
    log: Arc<PartitionLog>,
    fetcher: impl ReplicaFetcher,
) {
    loop {
        let fetch_offset = log.log_end_offset();

        match fetcher
            .fetch(&topic, partition, fetch_offset, MAX_FETCH_BYTES)
            .await
        {
            Ok(result) if result.records.is_empty() => {
                // No new records — back off briefly before polling again.
                tokio::time::sleep(std::time::Duration::from_millis(POLL_INTERVAL_MS)).await;
            }
            Ok(result) => {
                for record in result.records {
                    if let Err(e) = log.append(record.key, record.value) {
                        log::error!("replication append error {topic}-{partition}: {e}");
                        tokio::time::sleep(std::time::Duration::from_millis(BACKOFF_MS)).await;
                        continue;
                    }
                }
                // Advance follower HW to min(leader_hw, our LEO).
                let our_leo = log.log_end_offset();
                let hw = result.leader_hw.min(our_leo);
                if let Err(e) = log.advance_hw(hw) {
                    log::warn!("advance_hw error {topic}-{partition}: {e}");
                }
            }
            Err(e) => {
                log::warn!("fetch error {topic}-{partition}: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(BACKOFF_MS)).await;
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};

    fn temp_log() -> Arc<PartitionLog> {
        let db = sled::Config::new().temporary(true).open().unwrap();
        let tree = db.open_tree("p").unwrap();
        Arc::new(PartitionLog::open(tree).unwrap())
    }

    /// A mock fetcher that returns a fixed sequence of records then returns
    /// empty forever.
    struct MockFetcher {
        /// Records to serve, indexed by fetch_offset.
        records: Vec<(Option<Vec<u8>>, Vec<u8>)>,
        // Tracks next expected fetch_offset for verification.
        next: Arc<AtomicI64>,
    }

    #[async_trait]
    impl ReplicaFetcher for MockFetcher {
        async fn fetch(
            &self,
            _topic: &str,
            _partition: i32,
            fetch_offset: i64,
            _max_bytes: i32,
        ) -> anyhow::Result<FetchResult> {
            let idx = fetch_offset as usize;
            if idx >= self.records.len() {
                return Ok(FetchResult {
                    records: vec![],
                    leader_hw: self.records.len() as i64,
                });
            }
            // Return one record at a time for simplicity.
            let (key, value) = self.records[idx].clone();
            self.next.store(fetch_offset + 1, Ordering::SeqCst);
            Ok(FetchResult {
                records: vec![FetchedRecord { key, value }],
                leader_hw: self.records.len() as i64,
            })
        }
    }

    #[tokio::test]
    async fn fetch_task_replicates_records_to_log() {
        let mgr = ReplicationManager::new();
        let log = temp_log();
        let next = Arc::new(AtomicI64::new(0));

        let fetcher = MockFetcher {
            records: vec![
                (None, b"m0".to_vec()),
                (None, b"m1".to_vec()),
                (None, b"m2".to_vec()),
            ],
            next: next.clone(),
        };

        mgr.start_fetch_task("t".into(), 0, log.clone(), fetcher)
            .await;

        // Give the task time to replicate all 3 records.
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            if log.log_end_offset() >= 3 {
                break;
            }
        }

        assert_eq!(log.log_end_offset(), 3);
        assert_eq!(log.high_watermark(), 3);

        mgr.stop_fetch_task("t", 0).await;
        assert_eq!(mgr.active_task_count().await, 0);
    }

    #[tokio::test]
    async fn start_fetch_task_replaces_existing_task() {
        let mgr = ReplicationManager::new();
        let log = temp_log();

        let fetcher1 = MockFetcher {
            records: vec![(None, b"old".to_vec())],
            next: Arc::new(AtomicI64::new(0)),
        };
        mgr.start_fetch_task("t".into(), 0, log.clone(), fetcher1)
            .await;
        assert_eq!(mgr.active_task_count().await, 1);

        // Replace with a new fetcher for the same partition.
        let fetcher2 = MockFetcher {
            records: vec![],
            next: Arc::new(AtomicI64::new(0)),
        };
        mgr.start_fetch_task("t".into(), 0, log.clone(), fetcher2)
            .await;
        // Still exactly one task for (t, 0).
        assert_eq!(mgr.active_task_count().await, 1);

        mgr.stop_all().await;
        assert_eq!(mgr.active_task_count().await, 0);
    }

    #[tokio::test]
    async fn multiple_partitions_run_independently() {
        let mgr = ReplicationManager::new();
        let log0 = temp_log();
        let log1 = temp_log();

        let mk_fetcher = |data: Vec<&'static [u8]>| MockFetcher {
            records: data.iter().map(|v| (None, v.to_vec())).collect(),
            next: Arc::new(AtomicI64::new(0)),
        };

        mgr.start_fetch_task("t".into(), 0, log0.clone(), mk_fetcher(vec![b"a", b"b"]))
            .await;
        mgr.start_fetch_task("t".into(), 1, log1.clone(), mk_fetcher(vec![b"c"]))
            .await;

        assert_eq!(mgr.active_task_count().await, 2);

        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            if log0.log_end_offset() >= 2 && log1.log_end_offset() >= 1 {
                break;
            }
        }
        assert_eq!(log0.log_end_offset(), 2);
        assert_eq!(log1.log_end_offset(), 1);

        mgr.stop_fetch_task("t", 0).await;
        assert_eq!(mgr.active_task_count().await, 1);

        mgr.stop_all().await;
        assert_eq!(mgr.active_task_count().await, 0);
    }
}
