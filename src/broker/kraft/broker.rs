use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::broker::server::consumer_group::ConsumerGroupCoordinator;
use crate::broker::controller::types::ControllerMetadata;
use crate::broker::error::{BrokerError, BrokerResult};
use crate::broker::kraft::isr_manager::IsrManager;
use crate::broker::kraft::partition_log::PartitionLog;
use crate::broker::kraft::replication_manager::ReplicationManager;
use crate::broker::storage::traits::{
    validate_topic_name, BrokerInfo, BrokerStorage, ClusterMetadata, GroupMember, StoredMessage,
};

// ── ControllerHandle trait ────────────────────────────────────────────────────

/// Abstraction over the controller Raft quorum.
///
/// The production implementation wraps `RaftStorage` (Phase 6).
/// Tests supply a `MockControllerHandle`.
#[async_trait]
pub trait ControllerHandle: Send + Sync {
    /// Snapshot of the current cluster metadata.
    async fn metadata(&self) -> ControllerMetadata;

    /// True when this node is the active controller (Raft leader).
    fn is_controller(&self) -> bool;

    /// Node ID of the current active controller.
    fn controller_node_id(&self) -> u64;

    /// API address of the active controller for client redirects.
    fn controller_api_addr(&self) -> Option<String>;

    async fn propose_create_topic(
        &self,
        topic: String,
        num_partitions: i32,
        replication_factor: u16,
    ) -> anyhow::Result<()>;

    async fn propose_partition_change(
        &self,
        topic: String,
        partition: i32,
        leader: u64,
        isr: Vec<u64>,
        replicas: Vec<u64>,
    ) -> anyhow::Result<()>;

    async fn propose_add_node(
        &self,
        node_id: u64,
        api_addr: String,
        rpc_addr: String,
    ) -> anyhow::Result<()>;

    async fn propose_remove_node(&self, node_id: u64) -> anyhow::Result<()>;

    /// Propose a heartbeat timestamp for `broker_id`.
    /// `timestamp_ms` must be captured at call site (not inside apply) for
    /// Raft determinism.
    async fn propose_broker_heartbeat(
        &self,
        broker_id: u64,
        timestamp_ms: u64,
    ) -> anyhow::Result<()>;

    /// Declare `broker_id` dead and atomically apply `new_assignments`.
    async fn propose_mark_broker_dead(
        &self,
        broker_id: u64,
        new_assignments: Vec<crate::broker::controller::types::PartitionAssignment>,
    ) -> anyhow::Result<()>;
}

// ── KRaftBroker ───────────────────────────────────────────────────────────────

/// KRaft-style broker: Raft handles metadata only; message data lives in
/// per-partition [`PartitionLog`]s backed by sled.
pub struct KRaftBroker {
    node_id: u64,
    /// Client-facing API address ("host:port"), returned by GetCoordinatorInfo.
    api_addr: String,
    controller: Arc<dyn ControllerHandle>,
    /// Local partition logs for partitions this broker holds (leader or follower).
    partition_store: Arc<RwLock<HashMap<(String, i32), Arc<PartitionLog>>>>,
    /// Consumer committed offsets — local, not replicated.
    committed_offsets: Arc<RwLock<HashMap<(String, String, i32), (i64, String)>>>,
    group_coordinator: ConsumerGroupCoordinator,
    pub replication_mgr: Arc<ReplicationManager>,
    pub isr_mgr: Arc<IsrManager>,
    sled_db: Arc<sled::Db>,
    /// Default replication factor for newly created topics.
    replication_factor: u16,
}

impl KRaftBroker {
    /// Create a new `KRaftBroker` and sync partition logs from current metadata.
    pub async fn new(
        node_id: u64,
        api_addr: String,
        controller: Arc<dyn ControllerHandle>,
        sled_db: Arc<sled::Db>,
        rebalance_timeout_ms: i64,
        isr_lag_max: i64,
        replication_factor: u16,
    ) -> Self {
        let isr_mgr = Arc::new(IsrManager::new(node_id, isr_lag_max, 30_000));
        let broker = Self {
            node_id,
            api_addr,
            controller,
            partition_store: Arc::new(RwLock::new(HashMap::new())),
            committed_offsets: Arc::new(RwLock::new(HashMap::new())),
            group_coordinator: ConsumerGroupCoordinator::with_rebalance_timeout(
                rebalance_timeout_ms,
            ),
            replication_mgr: Arc::new(ReplicationManager::new()),
            isr_mgr,
            sled_db,
            replication_factor,
        };
        broker.sync_partitions_from_metadata().await;
        broker
    }

    /// Open `PartitionLog` trees for all partitions currently assigned to this
    /// node and update the group coordinator's topic map.
    async fn sync_partitions_from_metadata(&self) {
        let meta = self.controller.metadata().await;
        for (topic, record) in &meta.topics {
            self.group_coordinator
                .update_topic(topic.clone(), record.num_partitions)
                .await;
            for p in 0..record.num_partitions {
                if meta
                    .partitions
                    .get(&(topic.clone(), p))
                    .map_or(false, |pr| pr.leader == self.node_id || pr.replicas.contains(&self.node_id))
                {
                    self.ensure_partition_log(topic, p).await;
                }
            }
        }
    }

    /// Get an existing `PartitionLog` or open a new sled tree for it.
    async fn ensure_partition_log(&self, topic: &str, partition: i32) -> Arc<PartitionLog> {
        {
            let store = self.partition_store.read().await;
            if let Some(log) = store.get(&(topic.to_string(), partition)) {
                return log.clone();
            }
        }
        let tree_name = format!("p:{}:{}", topic, partition);
        let tree = self.sled_db.open_tree(tree_name).unwrap_or_else(|e| {
            panic!("failed to open sled tree for {topic}-{partition}: {e}")
        });
        let log = Arc::new(PartitionLog::open(tree).unwrap_or_else(|e| {
            panic!("failed to open PartitionLog for {topic}-{partition}: {e}")
        }));
        self.partition_store
            .write()
            .await
            .insert((topic.to_string(), partition), log.clone());
        log
    }

    /// Called when the controller assigns leadership of a partition to this node.
    /// Opens the PartitionLog and registers with IsrManager.
    pub async fn on_became_leader(&self, topic: &str, partition: i32, isr: Vec<u64>) {
        let log = self.ensure_partition_log(topic, partition).await;
        self.isr_mgr
            .add_partition(
                topic.to_string(),
                partition,
                isr.clone(),
                isr,
                log,
            )
            .await;
    }

    /// Record that follower `replica_id` has fetched up to `fetch_offset`.
    /// Called by the Fetch RPC handler when `replica_id > 0`.
    pub async fn record_replica_fetch(
        &self,
        topic: &str,
        partition: i32,
        replica_id: u64,
        fetch_offset: i64,
    ) {
        self.isr_mgr
            .record_fetch_progress(topic, partition, replica_id, fetch_offset)
            .await;
    }
}

// ── BrokerStorage impl ────────────────────────────────────────────────────────

#[async_trait]
impl BrokerStorage for KRaftBroker {
    async fn create_topic(&self, topic: &str, num_partitions: i32) -> BrokerResult<()> {
        validate_topic_name(topic)?;
        if num_partitions <= 0 {
            return Err(BrokerError::Validation(
                "num_partitions must be > 0".to_string(),
            ));
        }

        let rf = self.replication_factor;

        self.controller
            .propose_create_topic(topic.to_string(), num_partitions, rf)
            .await
            .map_err(BrokerError::from)?;

        // Only the active controller assigns replicas.
        if self.controller.is_controller() {
            // Collect alive broker IDs sorted for deterministic assignment.
            let meta = self.controller.metadata().await;
            let mut broker_ids: Vec<u64> = meta.brokers.keys().copied().collect();
            broker_ids.sort();

            // Fall back to this node when no brokers are registered yet
            // (e.g. first-boot single-node before any RegisterBroker is applied).
            if broker_ids.is_empty() {
                broker_ids.push(self.node_id);
            }

            let n = broker_ids.len();
            // Clamp rf to the number of available brokers.
            let effective_rf = (rf as usize).min(n);

            for p in 0..num_partitions {
                // Round-robin: start offset shifts by 1 per partition.
                let start = (p as usize) % n;
                let replicas: Vec<u64> = (0..effective_rf)
                    .map(|i| broker_ids[(start + i) % n])
                    .collect();
                let leader = replicas[0];
                // Initial ISR = all replicas (they start fully in-sync).
                let isr = replicas.clone();

                self.controller
                    .propose_partition_change(
                        topic.to_string(),
                        p,
                        leader,
                        isr,
                        replicas,
                    )
                    .await
                    .map_err(BrokerError::from)?;
            }
        }

        // Open logs and register with IsrManager for partitions this node leads.
        let meta = self.controller.metadata().await;
        for p in 0..num_partitions {
            let pr = meta.partitions.get(&(topic.to_string(), p));
            if pr.map_or(false, |r| r.leader == self.node_id) {
                let isr = pr.map(|r| r.isr.clone()).unwrap_or_default();
                self.on_became_leader(topic, p, isr).await;
            }
        }

        self.group_coordinator
            .update_topic(topic.to_string(), num_partitions)
            .await;

        Ok(())
    }

    async fn get_topics(&self) -> Vec<String> {
        self.controller.metadata().await.topics.keys().cloned().collect()
    }

    async fn get_topic_partitions(&self, topic: &str) -> Option<Vec<i32>> {
        self.controller
            .metadata()
            .await
            .topics
            .get(topic)
            .map(|t| (0..t.num_partitions).collect())
    }

    async fn produce_message(
        &self,
        topic: &str,
        partition: i32,
        key: Option<Vec<u8>>,
        value: Vec<u8>,
    ) -> BrokerResult<i64> {
        validate_topic_name(topic)?;
        let meta = self.controller.metadata().await;
        let pr = meta
            .partitions
            .get(&(topic.to_string(), partition))
            .ok_or_else(|| {
                BrokerError::NotFound(format!("Unknown partition {topic}-{partition}"))
            })?;

        if pr.leader == 0 {
            return Err(BrokerError::Internal(
                "Partition has no leader assigned yet".to_string(),
            ));
        }

        if pr.leader != self.node_id {
            let leader_addr = meta.brokers.get(&pr.leader).map(|b| b.api_addr.clone());
            return Err(BrokerError::NotLeader { leader_addr });
        }

        let log = self.ensure_partition_log(topic, partition).await;
        let offset = log
            .append(key, value)
            .map_err(|e| BrokerError::Internal(e.to_string()))?;

        // Single-replica fast path: when the ISR is just the leader there are no
        // followers to wait for, so advance HW immediately to LEO.
        if pr.isr.len() == 1 && pr.isr[0] == self.node_id {
            let _ = log.advance_hw(offset + 1);
        }

        Ok(offset)
    }

    /// Produce and wait until the high-watermark has advanced past the written
    /// offset, confirming that all in-sync replicas have acknowledged the write.
    /// Times out after 5 seconds if replication stalls.
    async fn produce_message_acks_all(
        &self,
        topic: &str,
        partition: i32,
        key: Option<Vec<u8>>,
        value: Vec<u8>,
    ) -> BrokerResult<i64> {
        // Perform the leader check + append (reuses produce_message logic).
        let offset = self.produce_message(topic, partition, key, value).await?;

        // Subscribe to HW advances before checking current HW to avoid a race.
        let log = {
            self.partition_store
                .read()
                .await
                .get(&(topic.to_string(), partition))
                .cloned()
        };
        let Some(log) = log else {
            // Log disappeared — shouldn't happen, but treat as replicated.
            return Ok(offset);
        };

        let target_hw = offset + 1;
        // Fast path: HW already past this offset (single-replica or already caught up).
        if log.high_watermark() >= target_hw {
            return Ok(offset);
        }

        let mut hw_rx = log.subscribe_hw();
        tokio::time::timeout(std::time::Duration::from_secs(5), async move {
            loop {
                if *hw_rx.borrow() >= target_hw {
                    return Ok(offset);
                }
                hw_rx
                    .changed()
                    .await
                    .map_err(|_| BrokerError::Internal("HW watch closed unexpectedly".into()))?;
            }
        })
        .await
        .unwrap_or_else(|_| {
            Err(BrokerError::Internal(
                "acks=all timeout: replication did not complete within 5 s".into(),
            ))
        })
    }

    async fn fetch_messages(
        &self,
        topic: &str,
        partition: i32,
        offset: i64,
        max_bytes: i32,
    ) -> BrokerResult<Vec<StoredMessage>> {
        let store = self.partition_store.read().await;
        let Some(log) = store.get(&(topic.to_string(), partition)) else {
            return Ok(vec![]);
        };
        log.read(offset, max_bytes)
            .map_err(|e| BrokerError::Internal(e.to_string()))
    }

    async fn get_partition_offset(
        &self,
        topic: &str,
        partition: i32,
        time: i64,
    ) -> BrokerResult<Vec<i64>> {
        let store = self.partition_store.read().await;
        let Some(log) = store.get(&(topic.to_string(), partition)) else {
            return Ok(vec![0]);
        };
        let offset = match time {
            -1 => log.log_end_offset(),
            -2 => log
                .first_offset()
                .map_err(|e| BrokerError::Internal(e.to_string()))?
                .unwrap_or(0),
            _ => log.log_end_offset(),
        };
        Ok(vec![offset])
    }

    async fn commit_offset(
        &self,
        group: &str,
        topic: &str,
        partition: i32,
        offset: i64,
        metadata: String,
    ) -> BrokerResult<()> {
        self.committed_offsets
            .write()
            .await
            .insert((group.to_string(), topic.to_string(), partition), (offset, metadata));
        Ok(())
    }

    async fn fetch_offset(
        &self,
        group: &str,
        topic: &str,
        partition: i32,
    ) -> BrokerResult<(i64, String)> {
        Ok(self
            .committed_offsets
            .read()
            .await
            .get(&(group.to_string(), topic.to_string(), partition))
            .cloned()
            .unwrap_or((-1, String::new())))
    }

    async fn get_coordinator_info(&self) -> (i32, String, i32) {
        let (host, port) = parse_api_addr(&self.api_addr);
        (self.node_id as i32, host, port)
    }

    async fn join_group(
        &self,
        group_id: &str,
        member_id: &str,
        _protocol_type: &str,
        protocol_metadata: &[u8],
        session_timeout_ms: i64,
    ) -> BrokerResult<(i32, String, String, Vec<GroupMember>)> {
        self.group_coordinator
            .join_group(group_id, member_id, protocol_metadata, session_timeout_ms)
            .await
    }

    async fn sync_group(
        &self,
        group_id: &str,
        _generation_id: i32,
        member_id: &str,
    ) -> BrokerResult<Vec<u8>> {
        self.group_coordinator.sync_group(group_id, member_id).await
    }

    async fn heartbeat(
        &self,
        group_id: &str,
        _generation_id: i32,
        member_id: &str,
    ) -> BrokerResult<()> {
        self.group_coordinator.heartbeat(group_id, member_id).await
    }

    async fn leave_group(&self, group_id: &str, member_id: &str) -> BrokerResult<()> {
        self.group_coordinator.leave_group(group_id, member_id).await
    }

    async fn get_cluster_metadata(&self) -> ClusterMetadata {
        let meta = self.controller.metadata().await;
        let controller_id = self.controller.controller_node_id();
        let brokers = meta
            .brokers
            .values()
            .map(|b| BrokerInfo {
                node_id: b.broker_id,
                api_addr: b.api_addr.clone(),
            })
            .collect();
        ClusterMetadata {
            brokers,
            leader_id: controller_id,
        }
    }

    async fn add_node(
        &self,
        node_id: u64,
        api_addr: String,
        rpc_addr: String,
    ) -> BrokerResult<()> {
        self.controller
            .propose_add_node(node_id, api_addr, rpc_addr)
            .await
            .map_err(BrokerError::from)
    }

    async fn remove_node(&self, node_id: u64) -> BrokerResult<()> {
        self.controller
            .propose_remove_node(node_id)
            .await
            .map_err(BrokerError::from)
    }
}

fn parse_api_addr(addr: &str) -> (String, i32) {
    if let Some((host, port_str)) = addr.rsplit_once(':') {
        if let Ok(port) = port_str.parse::<i32>() {
            return (host.to_string(), port);
        }
    }
    (addr.to_string(), 0)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::controller::state_machine::apply_controller_command;
    use crate::broker::controller::types::{BrokerRegistration, ControllerCommand};

    // ── MockControllerHandle ─────────────────────────────────────────────────

    /// Simulates instant Raft commits for unit testing.
    struct MockController {
        meta: Arc<RwLock<ControllerMetadata>>,
        node_id: u64,
        is_controller: bool,
        api_addr: String,
    }

    impl MockController {
        fn new(node_id: u64, api_addr: &str) -> Arc<Self> {
            Arc::new(Self {
                meta: Arc::new(RwLock::new(ControllerMetadata::default())),
                node_id,
                is_controller: true,
                api_addr: api_addr.to_string(),
            })
        }

        /// Pre-seed a topic with partitions already assigned to `node_id`.
        async fn seed_topic(&self, topic: &str, num_partitions: i32) {
            let mut meta = self.meta.write().await;
            apply_controller_command(
                &mut meta,
                ControllerCommand::CreateTopic {
                    topic: topic.to_string(),
                    num_partitions,
                    replication_factor: 1,
                },
            );
            for p in 0..num_partitions {
                apply_controller_command(
                    &mut meta,
                    ControllerCommand::PartitionChange {
                        topic: topic.to_string(),
                        partition: p,
                        leader: self.node_id,
                        isr: vec![self.node_id],
                        replicas: vec![self.node_id],
                    },
                );
            }
            apply_controller_command(
                &mut meta,
                ControllerCommand::RegisterBroker {
                    broker_id: self.node_id,
                    api_addr: self.api_addr.clone(),
                    rpc_addr: "127.0.0.1:9093".to_string(),
                },
            );
        }
    }

    #[async_trait]
    impl ControllerHandle for MockController {
        async fn metadata(&self) -> ControllerMetadata {
            self.meta.read().await.clone()
        }

        fn is_controller(&self) -> bool {
            self.is_controller
        }

        fn controller_node_id(&self) -> u64 {
            self.node_id
        }

        fn controller_api_addr(&self) -> Option<String> {
            Some(self.api_addr.clone())
        }

        async fn propose_create_topic(
            &self,
            topic: String,
            num_partitions: i32,
            replication_factor: u16,
        ) -> anyhow::Result<()> {
            let mut meta = self.meta.write().await;
            apply_controller_command(
                &mut meta,
                ControllerCommand::CreateTopic {
                    topic,
                    num_partitions,
                    replication_factor,
                },
            );
            Ok(())
        }

        async fn propose_partition_change(
            &self,
            topic: String,
            partition: i32,
            leader: u64,
            isr: Vec<u64>,
            replicas: Vec<u64>,
        ) -> anyhow::Result<()> {
            let mut meta = self.meta.write().await;
            apply_controller_command(
                &mut meta,
                ControllerCommand::PartitionChange {
                    topic,
                    partition,
                    leader,
                    isr,
                    replicas,
                },
            );
            Ok(())
        }

        async fn propose_add_node(
            &self,
            node_id: u64,
            api_addr: String,
            rpc_addr: String,
        ) -> anyhow::Result<()> {
            let mut meta = self.meta.write().await;
            meta.brokers.insert(
                node_id,
                BrokerRegistration {
                    broker_id: node_id,
                    api_addr,
                    rpc_addr,
                    last_seen_ms: 0,
                },
            );
            Ok(())
        }

        async fn propose_remove_node(&self, node_id: u64) -> anyhow::Result<()> {
            self.meta.write().await.brokers.remove(&node_id);
            Ok(())
        }

        async fn propose_broker_heartbeat(
            &self,
            _broker_id: u64,
            _timestamp_ms: u64,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn propose_mark_broker_dead(
            &self,
            broker_id: u64,
            new_assignments: Vec<crate::broker::controller::types::PartitionAssignment>,
        ) -> anyhow::Result<()> {
            use crate::broker::controller::state_machine::apply_controller_command;
            use crate::broker::controller::types::ControllerCommand;
            let mut meta = self.meta.write().await;
            apply_controller_command(
                &mut meta,
                ControllerCommand::MarkBrokerDead {
                    broker_id,
                    new_assignments,
                },
            );
            Ok(())
        }
    }

    // ── Test helpers ─────────────────────────────────────────────────────────

    async fn make_broker(node_id: u64) -> (KRaftBroker, Arc<MockController>) {
        let ctrl = MockController::new(node_id, "127.0.0.1:9092");
        let db = Arc::new(sled::Config::new().temporary(true).open().unwrap());
        let broker = KRaftBroker::new(
            node_id,
            "127.0.0.1:9092".to_string(),
            ctrl.clone(),
            db,
            30_000,
            1_000,
            1,
        )
        .await;
        (broker, ctrl)
    }

    async fn make_broker_with_topic(
        node_id: u64,
        topic: &str,
        num_partitions: i32,
    ) -> (KRaftBroker, Arc<MockController>) {
        let ctrl = MockController::new(node_id, "127.0.0.1:9092");
        ctrl.seed_topic(topic, num_partitions).await;
        let db = Arc::new(sled::Config::new().temporary(true).open().unwrap());
        let broker = KRaftBroker::new(
            node_id,
            "127.0.0.1:9092".to_string(),
            ctrl.clone(),
            db,
            30_000,
            1_000,
            1,
        )
        .await;
        (broker, ctrl)
    }

    // ── Topic tests ───────────────────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn create_topic_and_list() {
        let (broker, _ctrl) = make_broker(1).await;
        broker.create_topic("events", 3).await.unwrap();
        let topics = broker.get_topics().await;
        assert!(topics.contains(&"events".to_string()));
        let partitions = broker.get_topic_partitions("events").await.unwrap();
        assert_eq!(partitions, vec![0, 1, 2]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_topic_rejects_invalid_name() {
        let (broker, _) = make_broker(1).await;
        let err = broker.create_topic("bad:name", 1).await.unwrap_err();
        assert!(matches!(err, BrokerError::Validation(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_topic_rejects_zero_partitions() {
        let (broker, _) = make_broker(1).await;
        let err = broker.create_topic("t", 0).await.unwrap_err();
        assert!(matches!(err, BrokerError::Validation(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_topic_round_robin_assignment() {
        let ctrl = MockController::new(1, "127.0.0.1:9092");
        for id in [1u64, 2, 3] {
            ctrl.meta.write().await.brokers.insert(
                id,
                BrokerRegistration {
                    broker_id: id,
                    api_addr: format!("127.0.0.1:{}", 9000 + id),
                    rpc_addr: format!("127.0.0.1:{}", 9100 + id),
                    last_seen_ms: 0,
                },
            );
        }
        let db = Arc::new(sled::Config::new().temporary(true).open().unwrap());
        let broker = KRaftBroker::new(
            1,
            "127.0.0.1:9092".to_string(),
            ctrl.clone(),
            db,
            30_000,
            1_000,
            2, // replication_factor = 2
        )
        .await;

        broker.create_topic("t", 6).await.unwrap();

        let meta = ctrl.metadata().await;
        let expected_leaders = [1u64, 2, 3, 1, 2, 3];
        let expected_replicas: &[&[u64]] = &[
            &[1, 2], &[2, 3], &[3, 1], &[1, 2], &[2, 3], &[3, 1],
        ];
        for p in 0..6i32 {
            let pr = meta.partitions.get(&("t".to_string(), p)).unwrap();
            assert_eq!(pr.leader, expected_leaders[p as usize], "p{} leader", p);
            assert_eq!(&pr.replicas, expected_replicas[p as usize], "p{} replicas", p);
            assert_eq!(&pr.isr, expected_replicas[p as usize], "p{} isr", p);
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_topic_rf_clamped_to_broker_count() {
        let ctrl = MockController::new(1, "127.0.0.1:9092");
        for id in [1u64, 2] {
            ctrl.meta.write().await.brokers.insert(
                id,
                BrokerRegistration {
                    broker_id: id,
                    api_addr: format!("127.0.0.1:{}", 9000 + id),
                    rpc_addr: format!("127.0.0.1:{}", 9100 + id),
                    last_seen_ms: 0,
                },
            );
        }
        let db = Arc::new(sled::Config::new().temporary(true).open().unwrap());
        let broker = KRaftBroker::new(
            1,
            "127.0.0.1:9092".to_string(),
            ctrl.clone(),
            db,
            30_000,
            1_000,
            3, // rf=3 > 2 brokers → clamped to 2
        )
        .await;

        broker.create_topic("t", 2).await.unwrap();

        let meta = ctrl.metadata().await;
        for p in 0..2i32 {
            let pr = meta.partitions.get(&("t".to_string(), p)).unwrap();
            assert_eq!(pr.replicas.len(), 2, "p{} should have 2 replicas", p);
        }
    }

    // ── Produce / fetch tests ─────────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn produce_and_fetch_basic() {
        let (broker, _) = make_broker_with_topic(1, "t", 1).await;
        broker.on_became_leader("t", 0, vec![1]).await;

        let off0 = broker
            .produce_message("t", 0, None, b"hello".to_vec())
            .await
            .unwrap();
        let off1 = broker
            .produce_message("t", 0, None, b"world".to_vec())
            .await
            .unwrap();
        assert_eq!(off0, 0);
        assert_eq!(off1, 1);

        // Advance HW so fetch can see the messages
        {
            let store = broker.partition_store.read().await;
            let log = store.get(&("t".to_string(), 0)).unwrap();
            log.advance_hw(2).unwrap();
        }

        let msgs = broker.fetch_messages("t", 0, 0, 65536).await.unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].value, b"hello");
        assert_eq!(msgs[1].value, b"world");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn produce_to_non_leader_returns_not_leader() {
        let ctrl = MockController::new(1, "127.0.0.1:9092");
        {
            let mut meta = ctrl.meta.write().await;
            apply_controller_command(
                &mut meta,
                ControllerCommand::CreateTopic {
                    topic: "t".into(),
                    num_partitions: 1,
                    replication_factor: 2,
                },
            );
            apply_controller_command(
                &mut meta,
                ControllerCommand::PartitionChange {
                    topic: "t".into(),
                    partition: 0,
                    leader: 2,
                    isr: vec![2],
                    replicas: vec![1, 2],
                },
            );
            meta.brokers.insert(
                2,
                BrokerRegistration {
                    broker_id: 2,
                    api_addr: "127.0.0.1:9094".to_string(),
                    rpc_addr: "127.0.0.1:9095".to_string(),
                    last_seen_ms: 0,
                },
            );
        }
        let db = Arc::new(sled::Config::new().temporary(true).open().unwrap());
        let broker = KRaftBroker::new(
            1,
            "127.0.0.1:9092".to_string(),
            ctrl,
            db,
            30_000,
            1_000,
            1,
        )
        .await;

        let err = broker
            .produce_message("t", 0, None, b"x".to_vec())
            .await
            .unwrap_err();
        match err {
            BrokerError::NotLeader { leader_addr: Some(addr) } => {
                assert_eq!(addr, "127.0.0.1:9094");
            }
            other => panic!("expected NotLeader, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn offsets_are_sequential_after_topic_creation() {
        let (broker, _) = make_broker(1).await;
        broker.create_topic("seq", 1).await.unwrap();

        for i in 0..5u8 {
            let off = broker
                .produce_message("seq", 0, None, vec![i])
                .await
                .unwrap();
            assert_eq!(off, i as i64);
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_partition_offset_latest_equals_leo() {
        let (broker, _) = make_broker_with_topic(1, "t", 1).await;
        broker.on_became_leader("t", 0, vec![1]).await;

        broker
            .produce_message("t", 0, None, b"a".to_vec())
            .await
            .unwrap();
        broker
            .produce_message("t", 0, None, b"b".to_vec())
            .await
            .unwrap();

        let latest = broker.get_partition_offset("t", 0, -1).await.unwrap();
        assert_eq!(latest, vec![2]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_partition_offset_earliest_returns_first() {
        let (broker, _) = make_broker_with_topic(1, "t", 1).await;
        broker.on_became_leader("t", 0, vec![1]).await;

        broker
            .produce_message("t", 0, None, b"a".to_vec())
            .await
            .unwrap();

        let earliest = broker.get_partition_offset("t", 0, -2).await.unwrap();
        assert_eq!(earliest, vec![0]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn commit_and_fetch_offset() {
        let (broker, _) = make_broker(1).await;
        broker
            .commit_offset("grp", "t", 0, 42, "meta".to_string())
            .await
            .unwrap();
        let (off, meta) = broker.fetch_offset("grp", "t", 0).await.unwrap();
        assert_eq!(off, 42);
        assert_eq!(meta, "meta");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fetch_offset_returns_minus_one_when_none() {
        let (broker, _) = make_broker(1).await;
        let (off, _) = broker.fetch_offset("grp", "t", 0).await.unwrap();
        assert_eq!(off, -1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn join_group_delegates_to_coordinator() {
        let (broker, _) = make_broker_with_topic(1, "t", 2).await;
        let (gen, leader, member_id, members) = broker
            .join_group("g1", "m1", "consumer", b"t", 30_000)
            .await
            .unwrap();
        assert_eq!(gen, 0);
        assert_eq!(leader, "m1");
        assert_eq!(member_id, "m1");
        assert_eq!(members.len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_cluster_metadata_reflects_brokers() {
        let ctrl = MockController::new(1, "127.0.0.1:9092");
        {
            let mut meta = ctrl.meta.write().await;
            apply_controller_command(
                &mut meta,
                ControllerCommand::RegisterBroker {
                    broker_id: 1,
                    api_addr: "127.0.0.1:9092".to_string(),
                    rpc_addr: "127.0.0.1:9093".to_string(),
                },
            );
            apply_controller_command(
                &mut meta,
                ControllerCommand::RegisterBroker {
                    broker_id: 2,
                    api_addr: "127.0.0.1:9094".to_string(),
                    rpc_addr: "127.0.0.1:9095".to_string(),
                },
            );
        }
        let db = Arc::new(sled::Config::new().temporary(true).open().unwrap());
        let broker = KRaftBroker::new(1, "127.0.0.1:9092".to_string(), ctrl, db, 30_000, 1_000, 1)
            .await;

        let meta = broker.get_cluster_metadata().await;
        assert_eq!(meta.brokers.len(), 2);
        assert_eq!(meta.leader_id, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn produce_acks_all_single_replica_resolves_immediately() {
        let (broker, _) = make_broker_with_topic(1, "t", 1).await;
        broker.on_became_leader("t", 0, vec![1]).await;
        broker.isr_mgr.tick().await;

        let offset = broker
            .produce_message_acks_all("t", 0, None, b"hello".to_vec())
            .await
            .unwrap();
        assert_eq!(offset, 0);

        let store = broker.partition_store.read().await;
        let log = store.get(&("t".to_string(), 0)).unwrap();
        assert!(log.high_watermark() >= 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn produce_acks_all_waits_for_hw_advance() {
        let (broker, _) = make_broker_with_topic(1, "t", 1).await;
        broker.on_became_leader("t", 0, vec![1]).await;

        let log = {
            let store = broker.partition_store.read().await;
            store.get(&("t".to_string(), 0)).cloned().unwrap()
        };

        let broker = Arc::new(broker);
        let b = broker.clone();
        let produce_handle = tokio::spawn(async move {
            b.produce_message_acks_all("t", 0, None, b"msg".to_vec()).await
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        log.advance_hw(1).unwrap();

        let offset = produce_handle.await.unwrap().unwrap();
        assert_eq!(offset, 0);
    }
}
