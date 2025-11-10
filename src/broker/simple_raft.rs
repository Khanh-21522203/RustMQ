use raft::{
    Config, Error as RaftError, prelude::*,
    storage::MemStorage, RawNode, StateRole,
};
use raft::eraftpb::{ConfState, Message};
use slog::{Drain, Logger, o};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use serde::{Serialize, Deserialize};

/// Simple Raft node implementation for multi-broker
pub struct SimpleRaftNode {
    node_id: u64,
    raw_node: RawNode<MemStorage>,
    storage: Arc<Mutex<BrokerData>>,
    peers: HashMap<u64, String>, // node_id -> address
    logger: Logger,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrokerData {
    pub messages: HashMap<String, Vec<Vec<u8>>>, // topic -> messages
    pub offsets: HashMap<String, i64>, // consumer_group -> offset
}

impl Default for BrokerData {
    fn default() -> Self {
        Self {
            messages: HashMap::new(),
            offsets: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BrokerCommand {
    Produce {
        topic: String,
        message: Vec<u8>,
    },
    CommitOffset {
        group: String,
        offset: i64,
    },
}

impl SimpleRaftNode {
    pub fn new(node_id: u64, peers: Vec<u64>) -> Result<Self, RaftError> {
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

        // Create ConfState with peers as voters
        let mut conf_state = ConfState::default();
        conf_state.voters = peers.clone();
        conf_state.learners = vec![];
        
        let storage = MemStorage::new_with_conf_state(conf_state);
        let raw_node = RawNode::new(&config, storage, &logger)?;

        Ok(Self {
            node_id,
            raw_node,
            storage: Arc::new(Mutex::new(BrokerData::default())),
            peers: HashMap::new(),
            logger,
        })
    }

    pub fn propose(&mut self, command: BrokerCommand) -> Result<(), RaftError> {
        let data = bincode::serialize(&command).unwrap();
        self.raw_node.propose(vec![], data)?;
        Ok(())
    }

    pub fn tick(&mut self) {
        self.raw_node.tick();
    }

    pub fn step(&mut self, msg: Message) -> Result<(), RaftError> {
        self.raw_node.step(msg)
    }

    pub fn ready(&mut self) -> Option<Ready> {
        if !self.raw_node.has_ready() {
            return None;
        }
        Some(self.raw_node.ready())
    }

    pub fn advance(&mut self, ready: Ready) {
        let mut storage = self.raw_node.raft.raft_log.store.wl();
        
        if !ready.entries().is_empty() {
            let entries = ready.entries();
            storage.append(entries).unwrap();
        }
        
        if let Some(hs) = ready.hs() {
            storage.set_hardstate(hs.clone());
        }
        
        if !ready.snapshot().is_empty() {
            storage.apply_snapshot(ready.snapshot().clone()).unwrap();
        }
        
        drop(storage);
        
        // Apply committed entries
        for entry in ready.committed_entries() {
            if entry.data.is_empty() {
                continue;
            }
            
            if let Ok(command) = bincode::deserialize::<BrokerCommand>(&entry.data) {
                self.apply_command(command);
            }
        }
        
        self.raw_node.advance(ready);
    }

    fn apply_command(&mut self, command: BrokerCommand) {
        let mut data = self.storage.lock().unwrap();
        
        match command {
            BrokerCommand::Produce { topic, message } => {
                data.messages.entry(topic).or_insert_with(Vec::new).push(message);
            }
            BrokerCommand::CommitOffset { group, offset } => {
                data.offsets.insert(group, offset);
            }
        }
    }

    pub fn is_leader(&self) -> bool {
        self.raw_node.raft.state == StateRole::Leader
    }

    pub fn get_data(&self) -> BrokerData {
        self.storage.lock().unwrap().clone()
    }

    pub fn campaign(&mut self) -> Result<(), RaftError> {
        self.raw_node.campaign()
    }
}

/// Simple Raft-based broker storage adapter
pub struct SimpleRaftStorage {
    node: Arc<Mutex<SimpleRaftNode>>,
    tx: mpsc::Sender<BrokerCommand>,
}

impl SimpleRaftStorage {
    pub fn new(node_id: u64, peers: Vec<u64>) -> Result<Self, String> {
        let node = SimpleRaftNode::new(node_id, peers)
            .map_err(|e| format!("Failed to create raft node: {:?}", e))?;
        let node = Arc::new(Mutex::new(node));
        
        let (tx, mut rx) = mpsc::channel(100);
        
        // Start background task for Raft
        let node_clone = node.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(100));
            
            loop {
                interval.tick().await;
                
                let mut node = node_clone.lock().unwrap();
                node.tick();
                
                if let Some(ready) = node.ready() {
                    // Send messages to peers would go here
                    node.advance(ready);
                }
            }
        });
        
        // Handle commands
        let node_clone = node.clone();
        tokio::spawn(async move {
            while let Some(cmd) = rx.recv().await {
                let mut node = node_clone.lock().unwrap();
                if node.is_leader() {
                    let _ = node.propose(cmd);
                }
            }
        });
        
        Ok(Self { node, tx })
    }

    pub async fn produce(&self, topic: String, message: Vec<u8>) -> Result<(), String> {
        self.tx.send(BrokerCommand::Produce { topic, message }).await
            .map_err(|e| e.to_string())
    }

    pub async fn commit_offset(&self, group: String, offset: i64) -> Result<(), String> {
        self.tx.send(BrokerCommand::CommitOffset { group, offset }).await
            .map_err(|e| e.to_string())
    }

    pub fn get_messages(&self, topic: &str) -> Vec<Vec<u8>> {
        let node = self.node.lock().unwrap();
        let data = node.get_data();
        data.messages.get(topic).cloned().unwrap_or_default()
    }

    pub fn get_offset(&self, group: &str) -> Option<i64> {
        let node = self.node.lock().unwrap();
        let data = node.get_data();
        data.offsets.get(group).copied()
    }

    pub fn is_leader(&self) -> bool {
        self.node.lock().unwrap().is_leader()
    }
}
