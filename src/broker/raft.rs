use raft::{Config, storage::MemStorage, RawNode, StateRole};
use raft::eraftpb::{ConfState, Message};
use slog::{Drain, Logger, o};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, atomic::{AtomicBool, AtomicU64, Ordering}};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, oneshot, RwLock};
use serde::{Serialize, Deserialize};

use crate::broker::raft_network::{PeerInfo, RaftGrpcServer, RaftNetworkSender};

// ── Replicated state machine ──────────────────────────────────────────────────

/// The replicated state machine applied by Raft committed entries.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct BrokerData {
    /// (topic, partition) → ordered message log; index == offset
    pub messages: HashMap<(String, i32), Vec<BrokerStoredMessage>>,
    /// (group_id, topic, partition) → committed consumer offset
    pub offsets: HashMap<(String, String, i32), i64>,
    /// topic → num_partitions
    pub topics: HashMap<String, i32>,
    /// group_id → replicated group state
    pub groups: HashMap<String, ReplicatedGroupState>,
}

/// Serializable stored message for Raft log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrokerStoredMessage {
    pub offset: i64,
    pub key: Option<Vec<u8>>,
    pub value: Vec<u8>,
    pub timestamp_ms: i64,
}

// ── Consumer group state ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum GroupStatus {
    #[default]
    Empty,
    Stable,
    PreparingRebalance,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplicatedGroupMember {
    pub member_id: String,
    pub metadata: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReplicatedGroupState {
    pub generation_id: i32,
    pub leader_id: String,
    pub members: Vec<ReplicatedGroupMember>,
    pub subscriptions: HashMap<String, String>, // member_id → topic
    pub assignments: HashMap<String, Vec<i32>>, // member_id → partitions
    pub status: GroupStatus,
    pub rejoined: HashSet<String>,
    pub session_timeout_ms: i64,
    pub rebalance_started_ms: i64,
}

// ── Commands serialized into Raft log ─────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BrokerCommand {
    Produce {
        topic: String,
        partition: i32,
        key: Option<Vec<u8>>,
        value: Vec<u8>,
    },
    CommitOffset {
        group_id: String,
        topic: String,
        partition: i32,
        offset: i64,
    },
    CreateTopic {
        topic: String,
        num_partitions: i32,
    },
    TruncatePartition {
        topic: String,
        partition: i32,
        before_offset: i64,
    },
    GroupJoin {
        group_id: String,
        member_id: String,
        topic: String,
        metadata: Vec<u8>,
        session_timeout_ms: i64,
    },
    GroupLeave {
        group_id: String,
        member_id: String,
    },
    GroupExpire {
        group_id: String,
        expired_ids: Vec<String>,
    },
    GroupFinalize {
        group_id: String,
    },
}

// ── RaftNode ────────────────────────────────────────────────────────────

pub struct RaftNode {
    raw_node: RawNode<MemStorage>,
    data: Arc<RwLock<BrokerData>>,
    propose_rx: mpsc::UnboundedReceiver<(BrokerCommand, oneshot::Sender<anyhow::Result<i64>>)>,
    step_rx: mpsc::UnboundedReceiver<Message>,
    network: RaftNetworkSender,
    is_leader: Arc<AtomicBool>,
    leader_id: Arc<AtomicU64>,
    db: Option<Arc<sled::Db>>,
    #[allow(dead_code)]
    logger: Logger,
}

impl RaftNode {
    pub fn new(
        node_id: u64,
        peers: HashMap<u64, PeerInfo>,
        storage_path: Option<String>,
    ) -> Result<(Self, RaftStorage, RaftGrpcServer), raft::Error> {
        let decorator = slog_term::TermDecorator::new().build();
        let drain = slog_term::CompactFormat::new(decorator).build().fuse();
        let drain = slog_async::Async::new(drain).build().fuse();
        let logger = Logger::root(drain, o!("tag" => format!("node-{}", node_id)));

        let config = Config {
            id: node_id,
            election_tick: 10,
            heartbeat_tick: 3,
            ..Default::default()
        };

        let mut conf_state = ConfState::default();
        conf_state.voters = peers.keys().copied().collect();
        let storage = MemStorage::new_with_conf_state(conf_state);
        let raw_node = RawNode::new(&config, storage, &logger)?;

        let (db, initial_data) = match storage_path {
            Some(ref path) => {
                match sled::open(path) {
                    Ok(db) => {
                        let data = load_broker_data_from_sled(&db);
                        log::info!("Loaded BrokerData from sled at {}", path);
                        (Some(Arc::new(db)), data)
                    }
                    Err(e) => {
                        log::warn!("Failed to open sled at {}: {}", path, e);
                        (None, BrokerData::default())
                    }
                }
            }
            None => (None, BrokerData::default()),
        };

        let data = Arc::new(RwLock::new(initial_data));
        let is_leader = Arc::new(AtomicBool::new(false));
        let leader_id = Arc::new(AtomicU64::new(0));
        let (propose_tx, propose_rx) = mpsc::unbounded_channel();
        let (step_tx, step_rx) = mpsc::unbounded_channel();

        let network = RaftNetworkSender::new(node_id, peers.clone());
        let raft_grpc_server = RaftGrpcServer::new(step_tx);

        let node = Self {
            raw_node,
            data: data.clone(),
            propose_rx,
            step_rx,
            network,
            is_leader: is_leader.clone(),
            leader_id: leader_id.clone(),
            db,
            logger,
        };

        let raft_storage = RaftStorage {
            propose_tx,
            data,
            is_leader,
            leader_id,
            peers,
        };

        Ok((node, raft_storage, raft_grpc_server))
    }

    pub async fn run(mut self) {
        let mut tick_interval = tokio::time::interval(std::time::Duration::from_millis(100));

        loop {
            tokio::select! {
                _ = tick_interval.tick() => {
                    self.raw_node.tick();
                }
                Some((cmd, reply_tx)) = self.propose_rx.recv() => {
                    if self.raw_node.raft.state != StateRole::Leader {
                        let _ = reply_tx.send(Err(anyhow::anyhow!("not leader")));
                        continue;
                    }
                    match bincode::serialize(&cmd) {
                        Ok(data) => {
                            if let Err(e) = self.raw_node.propose(vec![], data) {
                                let _ = reply_tx.send(Err(anyhow::anyhow!("{:?}", e)));
                            } else {
                                let _ = reply_tx.send(Ok(0));
                            }
                        }
                        Err(e) => {
                            let _ = reply_tx.send(Err(anyhow::anyhow!("serialize: {}", e)));
                        }
                    }
                }
                Some(msg) = self.step_rx.recv() => {
                    let _ = self.raw_node.step(msg);
                }
            }

            if self.raw_node.has_ready() {
                self.handle_ready().await;
            }
        }
    }

    async fn handle_ready(&mut self) {
        let mut ready = self.raw_node.ready();

        {
            let mut store = self.raw_node.raft.raft_log.store.wl();
            if let Some(hs) = ready.hs() {
                store.set_hardstate(hs.clone());
            }
            if !ready.entries().is_empty() {
                if let Err(e) = store.append(ready.entries()) {
                    log::error!("Failed to append raft entries: {:?}", e);
                    return;
                }
            }
            if !ready.snapshot().is_empty() {
                if let Err(e) = store.apply_snapshot(ready.snapshot().clone()) {
                    log::error!("Failed to apply raft snapshot: {:?}", e);
                    return;
                }
            }
        }

        let msgs = ready.take_messages();
        self.network.send_messages(msgs).await;

        let committed: Vec<_> = ready.committed_entries().to_vec();
        for entry in committed {
            if entry.data.is_empty() { continue; }
            if let Ok(cmd) = bincode::deserialize::<BrokerCommand>(&entry.data) {
                self.apply_command(cmd).await;
            }
        }

        let mut light_rd = self.raw_node.advance(ready);

        let light_committed: Vec<_> = light_rd.committed_entries().to_vec();
        for entry in light_committed {
            if entry.data.is_empty() { continue; }
            if let Ok(cmd) = bincode::deserialize::<BrokerCommand>(&entry.data) {
                self.apply_command(cmd).await;
            }
        }

        let light_msgs = light_rd.take_messages();
        self.network.send_messages(light_msgs).await;

        self.raw_node.advance_apply();

        self.is_leader.store(
            self.raw_node.raft.state == StateRole::Leader,
            Ordering::SeqCst,
        );
        self.leader_id.store(self.raw_node.raft.leader_id, Ordering::SeqCst);
    }

    async fn apply_command(&self, cmd: BrokerCommand) -> i64 {
        let mut data = self.data.write().await;
        match cmd {
            BrokerCommand::Produce { topic, partition, key, value } => {
                let log = data.messages.entry((topic.clone(), partition)).or_default();
                let offset = log.len() as i64;
                let ts = now_ms();
                log.push(BrokerStoredMessage { offset, key: key.clone(), value: value.clone(), timestamp_ms: ts });
                if let Some(db) = &self.db {
                    let sled_key = format!("msg:{topic}:{partition:010}:{offset:020}");
                    let msg = BrokerStoredMessage { offset, key, value, timestamp_ms: ts };
                    if let Ok(bytes) = bincode::serialize(&msg) {
                        let _ = db.insert(sled_key.as_bytes(), bytes);
                    }
                }
                offset
            }
            BrokerCommand::CommitOffset { group_id, topic, partition, offset } => {
                data.offsets.insert((group_id.clone(), topic.clone(), partition), offset);
                if let Some(db) = &self.db {
                    let sled_key = format!("off:{group_id}:{topic}:{partition:010}");
                    if let Ok(bytes) = bincode::serialize(&offset) {
                        let _ = db.insert(sled_key.as_bytes(), bytes);
                    }
                }
                offset
            }
            BrokerCommand::CreateTopic { topic, num_partitions } => {
                data.topics.insert(topic.clone(), num_partitions);
                for p in 0..num_partitions {
                    data.messages.entry((topic.clone(), p)).or_default();
                }
                if let Some(db) = &self.db {
                    let sled_key = format!("top:{topic}");
                    if let Ok(bytes) = bincode::serialize(&num_partitions) {
                        let _ = db.insert(sled_key.as_bytes(), bytes);
                    }
                }
                0
            }
            BrokerCommand::TruncatePartition { topic, partition, before_offset } => {
                if let Some(log) = data.messages.get_mut(&(topic.clone(), partition)) {
                    log.retain(|m| m.offset >= before_offset);
                    if let Some(db) = &self.db {
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
            BrokerCommand::GroupJoin { group_id, member_id, topic, metadata, session_timeout_ms } => {
                // Snapshot topics before mutably borrowing groups
                let topics_snapshot = data.topics.clone();

                let group = data.groups.entry(group_id.clone()).or_insert_with(|| ReplicatedGroupState {
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
                    group.members.push(ReplicatedGroupMember { member_id: member_id.clone(), metadata });
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

                persist_group(&self.db, &group_id, group);
                0
            }
            BrokerCommand::GroupLeave { group_id, member_id } => {
                let topics_snapshot = data.topics.clone();
                if let Some(group) = data.groups.get_mut(&group_id) {
                    remove_member(group, &member_id);
                    if group.members.is_empty() {
                        group.status = GroupStatus::Empty;
                    } else {
                        trigger_rebalance(group, &member_id);
                    }
                    let _ = topics_snapshot; // unused in leave path (assignments recomputed at finalize)
                    let group_clone = group.clone();
                    persist_group(&self.db, &group_id, &group_clone);
                }
                0
            }
            BrokerCommand::GroupExpire { group_id, expired_ids } => {
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
                    persist_group(&self.db, &group_id, &group_clone);
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
                    persist_group(&self.db, &group_id, &group_clone);
                }
                0
            }
        }
    }
}

// ── Group helpers ─────────────────────────────────────────────────────────────

fn trigger_rebalance(group: &mut ReplicatedGroupState, joining_member_id: &str) {
    group.status = GroupStatus::PreparingRebalance;
    group.rejoined = HashSet::from([joining_member_id.to_string()]);
    group.generation_id += 1;
    group.rebalance_started_ms = now_ms();
}

fn finalize_rebalance(group: &mut ReplicatedGroupState, topics: &HashMap<String, i32>) {
    // Retain only members that rejoined
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
            let assigned: Vec<i32> = partitions.iter().enumerate()
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

/// Reconstruct BrokerData by scanning all sled keys.
fn load_broker_data_from_sled(db: &sled::Db) -> BrokerData {
    let mut data = BrokerData::default();

    for item in db.scan_prefix(b"msg:") {
        if let Ok((key, val)) = item {
            if let (Ok(key_str), Ok(msg)) = (
                std::str::from_utf8(&key),
                bincode::deserialize::<BrokerStoredMessage>(&val),
            ) {
                // key: "msg:{topic}:{partition:010}:{offset:020}"
                let parts: Vec<&str> = key_str.splitn(4, ':').collect();
                if parts.len() == 4 {
                    if let Ok(partition) = parts[2].parse::<i32>() {
                        data.messages.entry((parts[1].to_string(), partition)).or_default().push(msg);
                    }
                }
            }
        }
    }
    for log in data.messages.values_mut() {
        log.sort_by_key(|m| m.offset);
    }

    for item in db.scan_prefix(b"off:") {
        if let Ok((key, val)) = item {
            if let (Ok(key_str), Ok(offset)) = (
                std::str::from_utf8(&key),
                bincode::deserialize::<i64>(&val),
            ) {
                // key: "off:{group}:{topic}:{partition:010}"
                let parts: Vec<&str> = key_str.splitn(4, ':').collect();
                if parts.len() == 4 {
                    if let Ok(partition) = parts[3].parse::<i32>() {
                        data.offsets.insert(
                            (parts[1].to_string(), parts[2].to_string(), partition),
                            offset,
                        );
                    }
                }
            }
        }
    }

    for item in db.scan_prefix(b"top:") {
        if let Ok((key, val)) = item {
            if let (Ok(key_str), Ok(num_parts)) = (
                std::str::from_utf8(&key),
                bincode::deserialize::<i32>(&val),
            ) {
                // key: "top:{topic}"
                if let Some(topic) = key_str.strip_prefix("top:") {
                    data.topics.insert(topic.to_string(), num_parts);
                }
            }
        }
    }

    for item in db.scan_prefix(b"grp:") {
        if let Ok((key, val)) = item {
            if let (Ok(key_str), Ok(group)) = (
                std::str::from_utf8(&key),
                bincode::deserialize::<ReplicatedGroupState>(&val),
            ) {
                // key: "grp:{group_id}"
                if let Some(group_id) = key_str.strip_prefix("grp:") {
                    data.groups.insert(group_id.to_string(), group);
                }
            }
        }
    }

    data
}

// ── RaftStorage ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct RaftStorage {
    propose_tx: mpsc::UnboundedSender<(BrokerCommand, oneshot::Sender<anyhow::Result<i64>>)>,
    pub data: Arc<RwLock<BrokerData>>,
    is_leader: Arc<AtomicBool>,
    leader_id: Arc<AtomicU64>,
    peers: HashMap<u64, PeerInfo>,
}

impl RaftStorage {
    pub fn is_leader(&self) -> bool {
        self.is_leader.load(Ordering::SeqCst)
    }

    pub fn leader_api_addr(&self) -> Option<String> {
        let lid = self.leader_id.load(Ordering::SeqCst);
        if lid == 0 { return None; }
        self.peers.get(&lid).map(|p| p.api_addr.clone())
    }

    pub async fn read_data(&self) -> tokio::sync::RwLockReadGuard<'_, BrokerData> {
        self.data.read().await
    }

    async fn propose(&self, cmd: BrokerCommand) -> anyhow::Result<i64> {
        if !self.is_leader() {
            let addr = self.leader_api_addr().unwrap_or_default();
            anyhow::bail!("NOT_LEADER:{}", addr);
        }
        let (tx, rx) = oneshot::channel();
        self.propose_tx
            .send((cmd, tx))
            .map_err(|e| anyhow::anyhow!("channel closed: {}", e))?;
        rx.await.map_err(|e| anyhow::anyhow!("reply channel closed: {}", e))?
    }

    pub async fn propose_produce(&self, topic: String, partition: i32, key: Option<Vec<u8>>, value: Vec<u8>) -> anyhow::Result<i64> {
        self.propose(BrokerCommand::Produce { topic, partition, key, value }).await
    }

    pub async fn propose_commit_offset(&self, group_id: String, topic: String, partition: i32, offset: i64) -> anyhow::Result<()> {
        self.propose(BrokerCommand::CommitOffset { group_id, topic, partition, offset }).await?;
        Ok(())
    }

    pub async fn propose_create_topic(&self, topic: String, num_partitions: i32) -> anyhow::Result<()> {
        self.propose(BrokerCommand::CreateTopic { topic, num_partitions }).await?;
        Ok(())
    }

    pub async fn propose_truncate(&self, topic: String, partition: i32, before_offset: i64) -> anyhow::Result<()> {
        self.propose(BrokerCommand::TruncatePartition { topic, partition, before_offset }).await?;
        Ok(())
    }

    pub async fn propose_group_join(&self, group_id: String, member_id: String, topic: String, metadata: Vec<u8>, session_timeout_ms: i64) -> anyhow::Result<()> {
        self.propose(BrokerCommand::GroupJoin { group_id, member_id, topic, metadata, session_timeout_ms }).await?;
        Ok(())
    }

    pub async fn propose_group_leave(&self, group_id: String, member_id: String) -> anyhow::Result<()> {
        self.propose(BrokerCommand::GroupLeave { group_id, member_id }).await?;
        Ok(())
    }

    pub async fn propose_group_expire(&self, group_id: String, expired_ids: Vec<String>) -> anyhow::Result<()> {
        self.propose(BrokerCommand::GroupExpire { group_id, expired_ids }).await?;
        Ok(())
    }

    pub async fn propose_group_finalize(&self, group_id: String) -> anyhow::Result<()> {
        self.propose(BrokerCommand::GroupFinalize { group_id }).await?;
        Ok(())
    }
}
