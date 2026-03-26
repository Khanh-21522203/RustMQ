use raft::eraftpb::{ConfChange, ConfChangeType, ConfState, EntryType, Message};
use raft::{storage::MemStorage, Config, RawNode, StateRole};
use serde::{Deserialize, Serialize};
use slog::{o, Drain, Logger};
use std::collections::{HashMap, HashSet};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use tokio::sync::{mpsc, oneshot, RwLock};

use crate::broker::raft_network::{GrpcTransport, PeerInfo, RaftGrpcServer, RaftNetworkSender};
use crate::broker::raft_transport::RaftTransport;

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
    conf_change_rx: mpsc::UnboundedReceiver<(ConfChange, oneshot::Sender<anyhow::Result<()>>)>,
    step_rx: mpsc::UnboundedReceiver<Message>,
    network: Box<dyn RaftTransport>,
    is_leader: Arc<AtomicBool>,
    leader_id: Arc<AtomicU64>,
    /// Shared peer map — updated when ConfChange entries are applied.
    peers: Arc<std::sync::RwLock<HashMap<u64, PeerInfo>>>,
    pending_replies: HashMap<u64, oneshot::Sender<anyhow::Result<i64>>>,
    pending_cc_reply: Option<oneshot::Sender<anyhow::Result<()>>>,
    next_proposal_id: u64,
    db: Option<Arc<sled::Db>>,
    #[allow(dead_code)]
    logger: Logger,
}

impl RaftNode {
    /// Convenience constructor using the default gRPC transport.
    pub fn new(
        node_id: u64,
        peers: HashMap<u64, PeerInfo>,
        storage_path: Option<String>,
    ) -> Result<(Self, RaftStorage, RaftGrpcServer), raft::Error> {
        let peers_arc = Arc::new(std::sync::RwLock::new(peers));
        let (step_tx, step_rx) = mpsc::unbounded_channel();
        let grpc_server = RaftGrpcServer::new(step_tx.clone());
        let transport =
            Box::new(GrpcTransport(RaftNetworkSender::new(node_id, peers_arc.clone())));
        let (node, storage) = Self::new_with_transport(
            node_id,
            peers_arc,
            storage_path,
            transport,
            step_tx,
            step_rx,
        )?;
        Ok((node, storage, grpc_server))
    }

    /// Create a `RaftNode` with a pre-built transport and step channels.
    /// `peers` must be the same `Arc` that was passed to the transport constructor
    /// so that dynamic membership updates are reflected in both.
    pub fn new_with_transport(
        node_id: u64,
        peers: Arc<std::sync::RwLock<HashMap<u64, PeerInfo>>>,
        storage_path: Option<String>,
        transport: Box<dyn RaftTransport>,
        step_tx: mpsc::UnboundedSender<Message>,
        step_rx: mpsc::UnboundedReceiver<Message>,
    ) -> Result<(Self, RaftStorage), raft::Error> {
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

        // Open sled first so we can restore persisted peers before building ConfState.
        let (db, initial_data) = match storage_path {
            Some(ref path) => match sled::open(path) {
                Ok(db) => {
                    // Restore dynamically-added/removed peers from previous runs.
                    if let Some(saved_peers) = load_peers_from_sled(&db) {
                        log::info!("Restored {} peers from sled at {}", saved_peers.len(), path);
                        *peers.write().unwrap() = saved_peers;
                    }
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

        let mut conf_state = ConfState::default();
        conf_state.voters = peers.read().unwrap().keys().copied().collect();
        let storage = MemStorage::new_with_conf_state(conf_state);
        let raw_node = RawNode::new(&config, storage, &logger)?;

        let data = Arc::new(RwLock::new(initial_data));
        let is_leader = Arc::new(AtomicBool::new(false));
        let leader_id = Arc::new(AtomicU64::new(0));
        let (propose_tx, propose_rx) = mpsc::unbounded_channel();
        let (conf_change_tx, conf_change_rx) = mpsc::unbounded_channel();

        // step_tx is not used here; the server (gRPC or SBE+TCP) holds it externally.
        drop(step_tx);

        let node = Self {
            raw_node,
            data: data.clone(),
            propose_rx,
            conf_change_rx,
            step_rx,
            network: transport,
            is_leader: is_leader.clone(),
            leader_id: leader_id.clone(),
            peers: peers.clone(),
            pending_replies: HashMap::new(),
            pending_cc_reply: None,
            next_proposal_id: 1,
            db,
            logger,
        };

        let raft_storage = RaftStorage {
            propose_tx,
            conf_change_tx,
            data,
            is_leader,
            leader_id,
            peers,
        };

        Ok((node, raft_storage))
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
                        let _ = reply_tx.send(Err(self.not_leader_error()));
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
                Some((cc, reply_tx)) = self.conf_change_rx.recv() => {
                    if self.raw_node.raft.state != StateRole::Leader {
                        let _ = reply_tx.send(Err(self.not_leader_error()));
                        continue;
                    }
                    // Only one conf change in flight at a time
                    if self.pending_cc_reply.is_some() {
                        let _ = reply_tx.send(Err(anyhow::anyhow!("conf change already in progress")));
                        continue;
                    }
                    match self.raw_node.propose_conf_change(vec![], cc) {
                        Ok(_) => { self.pending_cc_reply = Some(reply_tx); }
                        Err(e) => { let _ = reply_tx.send(Err(anyhow::anyhow!("{:?}", e))); }
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

    fn not_leader_error(&self) -> anyhow::Error {
        let leader_id = self.raw_node.raft.leader_id;
        let leader_addr = self
            .peers
            .read()
            .unwrap()
            .get(&leader_id)
            .map(|p| p.api_addr.clone())
            .unwrap_or_default();
        anyhow::anyhow!("NOT_LEADER:{}", leader_addr)
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
            if entry.entry_type == EntryType::EntryConfChange as i32 {
                self.apply_conf_change_entry(&entry.data).await;
            } else if !entry.data.is_empty() {
                self.apply_entry_data(&entry.data).await;
            }
        }

        let mut light_rd = self.raw_node.advance(ready);

        let light_committed: Vec<_> = light_rd.committed_entries().to_vec();
        for entry in light_committed {
            if entry.entry_type == EntryType::EntryConfChange as i32 {
                self.apply_conf_change_entry(&entry.data).await;
            } else if !entry.data.is_empty() {
                self.apply_entry_data(&entry.data).await;
            }
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

    async fn apply_conf_change_entry(&mut self, data: &[u8]) {
        use prost_raft::Message as ProstRaftMsg;

        let cc = match <ConfChange as ProstRaftMsg>::decode(data) {
            Ok(cc) => cc,
            Err(e) => {
                log::error!("Failed to decode ConfChange: {}", e);
                // apply_conf_change MUST be called for every EntryConfChange to keep
                // raft-rs state consistent, even when we cannot parse the entry.
                let _ = self.raw_node.apply_conf_change(&ConfChange::default());
                if let Some(tx) = self.pending_cc_reply.take() {
                    let _ = tx.send(Err(anyhow::anyhow!("decode failed: {}", e)));
                }
                return;
            }
        };

        let cs = match self.raw_node.apply_conf_change(&cc) {
            Ok(cs) => cs,
            Err(e) => {
                log::error!("apply_conf_change failed: {:?}", e);
                if let Some(tx) = self.pending_cc_reply.take() {
                    let _ = tx.send(Err(anyhow::anyhow!("{:?}", e)));
                }
                return;
            }
        };

        // Persist new conf state
        self.raw_node.raft.raft_log.store.wl().set_conf_state(cs);

        // Update the shared peers map
        if cc.change_type == ConfChangeType::AddNode as i32 && cc.node_id != 0 {
            if let Ok(peer_info) = bincode::deserialize::<PeerInfo>(&cc.context) {
                self.peers.write().unwrap().insert(cc.node_id, peer_info);
                log::info!("Added node {} to peers map", cc.node_id);
            }
        } else if cc.change_type == ConfChangeType::RemoveNode as i32 {
            self.peers.write().unwrap().remove(&cc.node_id);
            log::info!("Removed node {} from peers map", cc.node_id);
        }

        // Persist the updated peers map so membership survives restarts.
        if let Some(ref db) = self.db {
            persist_peers_to_sled(db, &self.peers.read().unwrap());
        }

        if let Some(tx) = self.pending_cc_reply.take() {
            let _ = tx.send(Ok(()));
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
pub fn load_broker_data_from_sled(db: &sled::Db) -> BrokerData {
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

/// Persist the current peers map to sled under the key `"peers"`.
/// Called after every ConfChange so membership survives restarts.
fn persist_peers_to_sled(db: &sled::Db, peers: &HashMap<u64, PeerInfo>) {
    match bincode::serialize(peers) {
        Ok(bytes) => {
            if let Err(e) = db.insert(b"peers", bytes) {
                log::error!("Failed to persist peers to sled: {}", e);
            }
        }
        Err(e) => log::error!("Failed to serialize peers: {}", e),
    }
}

/// Load the peers map from sled.  Returns `None` if no entry exists yet.
fn load_peers_from_sled(db: &sled::Db) -> Option<HashMap<u64, PeerInfo>> {
    db.get(b"peers")
        .ok()
        .flatten()
        .and_then(|bytes| bincode::deserialize::<HashMap<u64, PeerInfo>>(&bytes).ok())
}

// ── RaftStorage ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct RaftStorage {
    propose_tx: mpsc::UnboundedSender<(BrokerCommand, oneshot::Sender<anyhow::Result<i64>>)>,
    conf_change_tx: mpsc::UnboundedSender<(ConfChange, oneshot::Sender<anyhow::Result<()>>)>,
    pub data: Arc<RwLock<BrokerData>>,
    is_leader: Arc<AtomicBool>,
    leader_id: Arc<AtomicU64>,
    peers: Arc<std::sync::RwLock<HashMap<u64, PeerInfo>>>,
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
        self.peers
            .read()
            .unwrap()
            .get(&lid)
            .map(|p| p.api_addr.clone())
    }

    /// Return a snapshot of all known peers for metadata reporting.
    pub fn peers_snapshot(&self) -> Vec<(u64, String)> {
        self.peers
            .read()
            .unwrap()
            .iter()
            .map(|(&id, p)| (id, p.api_addr.clone()))
            .collect()
    }

    pub fn current_leader_id(&self) -> u64 {
        self.leader_id.load(Ordering::SeqCst)
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

    /// Propose adding a new voter node to the Raft cluster.
    pub async fn propose_add_node(
        &self,
        node_id: u64,
        api_addr: String,
        rpc_addr: String,
    ) -> anyhow::Result<()> {
        if !self.is_leader() {
            let addr = self.leader_api_addr().unwrap_or_default();
            anyhow::bail!("NOT_LEADER:{}", addr);
        }
        // Ensure rpc_addr has the http:// scheme required by tonic's gRPC transport.
        // The SBE+TCP transport strips the scheme via bare_addr(), so it handles both.
        let rpc_addr = if rpc_addr.starts_with("http") {
            rpc_addr
        } else {
            format!("http://{}", rpc_addr)
        };
        let peer_info = PeerInfo {
            api_addr,
            rpc_addr,
            sbe_tcp_addr: None,
        };
        let context = bincode::serialize(&peer_info)
            .map_err(|e| anyhow::anyhow!("serialize PeerInfo: {}", e))?;
        let cc = ConfChange {
            change_type: ConfChangeType::AddNode as i32,
            node_id,
            context,
            ..Default::default()
        };
        let (tx, rx) = oneshot::channel();
        self.conf_change_tx
            .send((cc, tx))
            .map_err(|e| anyhow::anyhow!("channel closed: {}", e))?;
        rx.await
            .map_err(|e| anyhow::anyhow!("reply closed: {}", e))?
    }

    /// Propose removing a voter node from the Raft cluster.
    pub async fn propose_remove_node(&self, node_id: u64) -> anyhow::Result<()> {
        if !self.is_leader() {
            let addr = self.leader_api_addr().unwrap_or_default();
            anyhow::bail!("NOT_LEADER:{}", addr);
        }
        let cc = ConfChange {
            change_type: ConfChangeType::RemoveNode as i32,
            node_id,
            ..Default::default()
        };
        let (tx, rx) = oneshot::channel();
        self.conf_change_tx
            .send((cc, tx))
            .map_err(|e| anyhow::anyhow!("channel closed: {}", e))?;
        rx.await
            .map_err(|e| anyhow::anyhow!("reply closed: {}", e))?
    }
}
