use raft::{Config, storage::MemStorage, RawNode, StateRole};
use raft::eraftpb::ConfState;
use slog::{Drain, Logger, o};
use std::collections::HashMap;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use tokio::sync::{mpsc, oneshot, RwLock};
use serde::{Serialize, Deserialize};

// ── Replicated state machine ──────────────────────────────────────────────────

/// The replicated state machine applied by Raft committed entries.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct BrokerData {
    /// (topic, partition) → ordered message log; index == offset
    pub messages: HashMap<(String, i32), Vec<BrokerStoredMessage>>,
    /// (group_id, topic, partition) → committed consumer offset
    pub offsets: HashMap<(String, String, i32), i64>,
}

/// Serializable stored message for Raft log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrokerStoredMessage {
    pub offset: i64,
    pub key: Option<Vec<u8>>,
    pub value: Vec<u8>,
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
}

// ── SimpleRaftNode ────────────────────────────────────────────────────────────

pub struct SimpleRaftNode {
    node_id: u64,
    raw_node: RawNode<MemStorage>,
    data: Arc<RwLock<BrokerData>>,
    propose_rx: mpsc::UnboundedReceiver<(BrokerCommand, oneshot::Sender<anyhow::Result<i64>>)>,
    is_leader: Arc<AtomicBool>,
    logger: Logger,
}

impl SimpleRaftNode {
    pub fn new(
        node_id: u64,
        peers: Vec<u64>,
    ) -> Result<(Self, SimpleRaftStorage), raft::Error> {
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
        conf_state.voters = peers.clone();
        let storage = MemStorage::new_with_conf_state(conf_state);
        let raw_node = RawNode::new(&config, storage, &logger)?;

        let data = Arc::new(RwLock::new(BrokerData::default()));
        let is_leader = Arc::new(AtomicBool::new(false));
        let (propose_tx, propose_rx) = mpsc::unbounded_channel();

        let node = Self {
            node_id,
            raw_node,
            data: data.clone(),
            propose_rx,
            is_leader: is_leader.clone(),
            logger,
        };

        let storage = SimpleRaftStorage {
            propose_tx,
            data,
            is_leader,
        };

        Ok((node, storage))
    }

    /// Drive the Raft loop. Must be spawned on a Tokio task.
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
                                // Send Ok(0) after propose succeeds.
                                // The actual offset is applied via apply_command.
                                let _ = reply_tx.send(Ok(0));
                            }
                        }
                        Err(e) => {
                            let _ = reply_tx.send(Err(anyhow::anyhow!("serialize: {}", e)));
                        }
                    }
                }
            }

            if self.raw_node.has_ready() {
                self.handle_ready().await;
            }
        }
    }

    async fn handle_ready(&mut self) {
        let ready = self.raw_node.ready();

        // 1. Persist HardState first, then entries (correct ordering per Raft paper)
        {
            let mut store = self.raw_node.raft.raft_log.store.wl();
            if let Some(hs) = ready.hs() {
                store.set_hardstate(hs.clone());
            }
            if !ready.entries().is_empty() {
                store.append(ready.entries()).unwrap();
            }
            if !ready.snapshot().is_empty() {
                store.apply_snapshot(ready.snapshot().clone()).unwrap();
            }
        }

        // 2. Apply committed entries to BrokerData
        let committed: Vec<_> = ready.committed_entries().to_vec();
        for entry in committed {
            if entry.data.is_empty() {
                continue;
            }
            if let Ok(cmd) = bincode::deserialize::<BrokerCommand>(&entry.data) {
                self.apply_command(cmd).await;
            }
        }

        // 3. Advance and handle LightReady
        let light_rd = self.raw_node.advance(ready);

        // Apply any committed entries from LightReady
        let light_committed: Vec<_> = light_rd.committed_entries().to_vec();
        for entry in light_committed {
            if entry.data.is_empty() {
                continue;
            }
            if let Ok(cmd) = bincode::deserialize::<BrokerCommand>(&entry.data) {
                self.apply_command(cmd).await;
            }
        }

        self.raw_node.advance_apply();

        // 4. Update is_leader
        self.is_leader.store(
            self.raw_node.raft.state == StateRole::Leader,
            Ordering::SeqCst,
        );
    }

    async fn apply_command(&self, cmd: BrokerCommand) -> i64 {
        let mut data = self.data.write().await;
        match cmd {
            BrokerCommand::Produce { topic, partition, key, value } => {
                let log = data.messages.entry((topic, partition)).or_default();
                let offset = log.len() as i64;
                log.push(BrokerStoredMessage { offset, key, value });
                offset
            }
            BrokerCommand::CommitOffset { group_id, topic, partition, offset } => {
                data.offsets.insert((group_id, topic, partition), offset);
                offset
            }
        }
    }
}

// ── SimpleRaftStorage ─────────────────────────────────────────────────────────

/// Async adapter that MultiBroker uses to interact with SimpleRaftNode.
#[derive(Clone)]
pub struct SimpleRaftStorage {
    propose_tx: mpsc::UnboundedSender<(BrokerCommand, oneshot::Sender<anyhow::Result<i64>>)>,
    pub data: Arc<RwLock<BrokerData>>,
    is_leader: Arc<AtomicBool>,
}

impl SimpleRaftStorage {
    pub fn is_leader(&self) -> bool {
        self.is_leader.load(Ordering::SeqCst)
    }

    pub async fn read_data(&self) -> tokio::sync::RwLockReadGuard<'_, BrokerData> {
        self.data.read().await
    }

    pub async fn propose_produce(
        &self,
        topic: String,
        partition: i32,
        key: Option<Vec<u8>>,
        value: Vec<u8>,
    ) -> anyhow::Result<i64> {
        if !self.is_leader() {
            anyhow::bail!("not leader for partition");
        }
        let (tx, rx) = oneshot::channel();
        self.propose_tx
            .send((BrokerCommand::Produce { topic, partition, key, value }, tx))
            .map_err(|e| anyhow::anyhow!("channel closed: {}", e))?;
        rx.await.map_err(|e| anyhow::anyhow!("reply channel closed: {}", e))?
    }

    pub async fn propose_commit_offset(
        &self,
        group_id: String,
        topic: String,
        partition: i32,
        offset: i64,
    ) -> anyhow::Result<()> {
        if !self.is_leader() {
            anyhow::bail!("not leader for partition");
        }
        let (tx, rx) = oneshot::channel();
        self.propose_tx
            .send((BrokerCommand::CommitOffset { group_id, topic, partition, offset }, tx))
            .map_err(|e| anyhow::anyhow!("channel closed: {}", e))?;
        rx.await
            .map_err(|e| anyhow::anyhow!("reply channel closed: {}", e))??;
        Ok(())
    }
}
