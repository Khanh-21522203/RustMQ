use raft::eraftpb::{ConfState, Message};
use raft::{storage::MemStorage, Config, RawNode, StateRole};
use serde::{Deserialize, Serialize};
use slog::{o, Drain, Logger};
use std::collections::{HashMap, HashSet};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use tokio::sync::{mpsc, oneshot, RwLock};

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
    /// (topic, partition) → next offset watermark; monotonically increasing, survives truncation
    #[serde(default)]
    pub next_offsets: HashMap<(String, i32), i64>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RaftProposal {
    proposal_id: u64,
    command: BrokerCommand,
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
    pending_replies: HashMap<u64, oneshot::Sender<anyhow::Result<i64>>>,
    next_proposal_id: u64,
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
            Some(ref path) => match sled::open(path) {
                Ok(db) => {
                    let data = load_broker_data_from_sled(&db);
                    log::info!("Loaded BrokerData from sled at {}", path);
                    (Some(Arc::new(db)), data)
                }
                Err(e) => {
                    log::warn!("Failed to open sled at {}: {}", path, e);
                    (None, BrokerData::default())
                }
            },
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
            pending_replies: HashMap::new(),
            next_proposal_id: 1,
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
                    let proposal_id = self.next_proposal_id;
                    self.next_proposal_id = self.next_proposal_id.wrapping_add(1);
                    let proposal = RaftProposal { proposal_id, command: cmd };

                    match bincode::serialize(&proposal) {
                        Ok(data) => {
                            if let Err(e) = self.raw_node.propose(vec![], data) {
                                let _ = reply_tx.send(Err(anyhow::anyhow!("{:?}", e)));
                            } else {
                                self.pending_replies.insert(proposal_id, reply_tx);
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
        let persisted_msgs = ready.take_persisted_messages();
        self.network.send_messages(persisted_msgs).await;

        let committed: Vec<_> = ready.committed_entries().to_vec();
        for entry in committed {
            if entry.data.is_empty() {
                continue;
            }
            self.apply_entry_data(&entry.data).await;
        }

        let mut light_rd = self.raw_node.advance(ready);

        let light_committed: Vec<_> = light_rd.committed_entries().to_vec();
        for entry in light_committed {
            if entry.data.is_empty() {
                continue;
            }
            self.apply_entry_data(&entry.data).await;
        }

        let light_msgs = light_rd.take_messages();
        self.network.send_messages(light_msgs).await;

        self.raw_node.advance_apply();

        let was_leader = self.is_leader.load(Ordering::SeqCst);
        let is_leader = self.raw_node.raft.state == StateRole::Leader;
        self.is_leader.store(is_leader, Ordering::SeqCst);
        self.leader_id
            .store(self.raw_node.raft.leader_id, Ordering::SeqCst);
        if was_leader && !is_leader {
            self.fail_pending_replies("leadership changed before commit");
        }
    }

    async fn apply_entry_data(&mut self, data: &[u8]) {
        // Preferred format with proposal id for commit-aware acknowledgments.
        if let Ok(proposal) = bincode::deserialize::<RaftProposal>(data) {
            let result = self.apply_command(proposal.command).await;
            if let Some(tx) = self.pending_replies.remove(&proposal.proposal_id) {
                let _ = tx.send(Ok(result));
            }
            return;
        }

        // Backward compatibility: older entries serialized only BrokerCommand.
        if let Ok(cmd) = bincode::deserialize::<BrokerCommand>(data) {
            let _ = self.apply_command(cmd).await;
        }
    }

    fn fail_pending_replies(&mut self, reason: &str) {
        let pending = std::mem::take(&mut self.pending_replies);
        for (_, tx) in pending {
            let _ = tx.send(Err(anyhow::anyhow!(reason.to_string())));
        }
    }

    async fn apply_command(&self, cmd: BrokerCommand) -> i64 {
        let mut data = self.data.write().await;
        crate::broker::state_machine::apply_raft_command(&mut data, &self.db, cmd)
    }
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
                        data.messages
                            .entry((parts[1].to_string(), partition))
                            .or_default()
                            .push(msg);
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
            if let (Ok(key_str), Ok(offset)) =
                (std::str::from_utf8(&key), bincode::deserialize::<i64>(&val))
            {
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
            if let (Ok(key_str), Ok(num_parts)) =
                (std::str::from_utf8(&key), bincode::deserialize::<i32>(&val))
            {
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

    // Load high-watermark offsets; key: "hwm:{topic}:{partition:010}"
    for item in db.scan_prefix(b"hwm:") {
        if let Ok((key, val)) = item {
            if let (Ok(key_str), Ok(next)) =
                (std::str::from_utf8(&key), bincode::deserialize::<i64>(&val))
            {
                let parts: Vec<&str> = key_str.splitn(3, ':').collect();
                if parts.len() == 3 {
                    if let Ok(partition) = parts[2].parse::<i32>() {
                        let topic = parts[1].to_string();
                        let entry = data.next_offsets.entry((topic, partition)).or_insert(0);
                        *entry = (*entry).max(next);
                    }
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
        if lid == 0 {
            return None;
        }
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
        rx.await
            .map_err(|e| anyhow::anyhow!("reply channel closed: {}", e))?
    }

    pub async fn propose_produce(
        &self,
        topic: String,
        partition: i32,
        key: Option<Vec<u8>>,
        value: Vec<u8>,
    ) -> anyhow::Result<i64> {
        self.propose(BrokerCommand::Produce {
            topic,
            partition,
            key,
            value,
        })
        .await
    }

    pub async fn propose_commit_offset(
        &self,
        group_id: String,
        topic: String,
        partition: i32,
        offset: i64,
    ) -> anyhow::Result<()> {
        self.propose(BrokerCommand::CommitOffset {
            group_id,
            topic,
            partition,
            offset,
        })
        .await?;
        Ok(())
    }

    pub async fn propose_create_topic(
        &self,
        topic: String,
        num_partitions: i32,
    ) -> anyhow::Result<()> {
        self.propose(BrokerCommand::CreateTopic {
            topic,
            num_partitions,
        })
        .await?;
        Ok(())
    }

    pub async fn propose_truncate(
        &self,
        topic: String,
        partition: i32,
        before_offset: i64,
    ) -> anyhow::Result<()> {
        self.propose(BrokerCommand::TruncatePartition {
            topic,
            partition,
            before_offset,
        })
        .await?;
        Ok(())
    }

    pub async fn propose_group_join(
        &self,
        group_id: String,
        member_id: String,
        topic: String,
        metadata: Vec<u8>,
        session_timeout_ms: i64,
    ) -> anyhow::Result<()> {
        self.propose(BrokerCommand::GroupJoin {
            group_id,
            member_id,
            topic,
            metadata,
            session_timeout_ms,
        })
        .await?;
        Ok(())
    }

    pub async fn propose_group_leave(
        &self,
        group_id: String,
        member_id: String,
    ) -> anyhow::Result<()> {
        self.propose(BrokerCommand::GroupLeave {
            group_id,
            member_id,
        })
        .await?;
        Ok(())
    }

    pub async fn propose_group_expire(
        &self,
        group_id: String,
        expired_ids: Vec<String>,
    ) -> anyhow::Result<()> {
        self.propose(BrokerCommand::GroupExpire {
            group_id,
            expired_ids,
        })
        .await?;
        Ok(())
    }

    pub async fn propose_group_finalize(&self, group_id: String) -> anyhow::Result<()> {
        self.propose(BrokerCommand::GroupFinalize { group_id })
            .await?;
        Ok(())
    }
}
