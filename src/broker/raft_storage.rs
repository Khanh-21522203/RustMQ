use async_trait::async_trait;
use openraft::{
    storage::{LogState, RaftLogStorage, RaftStorage},
    Entry, EntryPayload, LogId, RaftLogReader, SnapshotMeta, StorageError, StoredMembership,
    Vote,
};
use rocksdb::{ColumnFamilyDescriptor, DB, Options};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt::Debug;
use std::io::Cursor;
use std::ops::RangeBounds;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::broker::raft_state_machine::BrokerStateMachine;
use crate::broker::raft_types::{BrokerNode, BrokerNodeId, TypeConfig};

const CF_LOGS: &str = "logs";
const CF_STATE: &str = "state";

/// Storage implementation for Raft using RocksDB
pub struct BrokerRaftStorage {
    /// RocksDB instance
    db: Arc<DB>,
    
    /// State machine
    state_machine: Arc<RwLock<BrokerStateMachine>>,
    
    /// Current vote
    vote: Arc<RwLock<Option<Vote<BrokerNodeId>>>>,
    
    /// Log state
    log_state: Arc<RwLock<LogState<TypeConfig>>>,
    
    /// Snapshot
    current_snapshot: Arc<RwLock<Option<StorageSnapshot>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StorageSnapshot {
    meta: SnapshotMeta<BrokerNodeId, BrokerNode>,
    data: Vec<u8>,
}

impl BrokerRaftStorage {
    pub fn new<P: AsRef<Path>>(db_path: P) -> Result<Self, Box<dyn std::error::Error>> {
        // Create RocksDB with column families
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);
        
        let cf_logs = ColumnFamilyDescriptor::new(CF_LOGS, Options::default());
        let cf_state = ColumnFamilyDescriptor::new(CF_STATE, Options::default());
        
        let db = DB::open_cf_descriptors(&opts, db_path, vec![cf_logs, cf_state])?;
        
        let storage = Self {
            db: Arc::new(db),
            state_machine: Arc::new(RwLock::new(BrokerStateMachine::new())),
            vote: Arc::new(RwLock::new(None)),
            log_state: Arc::new(RwLock::new(LogState::default())),
            current_snapshot: Arc::new(RwLock::new(None)),
        };
        
        // Load vote and log state from disk
        storage.load_state();
        
        Ok(storage)
    }
    
    fn load_state(&self) {
        // Load vote from state CF
        if let Ok(Some(data)) = self.db.get_cf(self.get_cf(CF_STATE), b"vote") {
            if let Ok(vote) = bincode::deserialize::<Vote<BrokerNodeId>>(&data) {
                let mut v = self.vote.blocking_write();
                *v = Some(vote);
            }
        }
    }
    
    fn get_cf(&self, name: &str) -> &rocksdb::ColumnFamily {
        self.db.cf_handle(name).expect("Column family should exist")
    }
    
    async fn get_log(&self, log_id: u64) -> Option<Entry<TypeConfig>> {
        let key = format!("log_{:020}", log_id);
        match self.db.get_cf(self.get_cf(CF_LOGS), key.as_bytes()) {
            Ok(Some(data)) => {
                bincode::deserialize(&data).ok()
            }
            _ => None,
        }
    }
    
    async fn save_log(&self, entry: &Entry<TypeConfig>) -> Result<(), StorageError<BrokerNodeId>> {
        let key = format!("log_{:020}", entry.log_id.index);
        let value = bincode::serialize(entry)
            .map_err(|e| StorageError::from_io_error(
                openraft::ErrorSubject::Log(entry.log_id),
                openraft::ErrorVerb::Write,
                std::io::Error::new(std::io::ErrorKind::InvalidData, e),
            ))?;
        
        self.db.put_cf(self.get_cf(CF_LOGS), key.as_bytes(), value)
            .map_err(|e| StorageError::from_io_error(
                openraft::ErrorSubject::Log(entry.log_id),
                openraft::ErrorVerb::Write,
                std::io::Error::new(std::io::ErrorKind::Other, e),
            ))?;
        
        Ok(())
    }
    
    async fn delete_logs_from(&self, log_id: u64) -> Result<(), StorageError<BrokerNodeId>> {
        // Delete all logs with index >= log_id
        let prefix = b"log_";
        let cf = self.get_cf(CF_LOGS);
        
        let mut batch = rocksdb::WriteBatch::default();
        let iter = self.db.prefix_iterator_cf(cf, prefix);
        
        for item in iter {
            if let Ok((key, _)) = item {
                if let Ok(key_str) = std::str::from_utf8(&key) {
                    if let Some(id_str) = key_str.strip_prefix("log_") {
                        if let Ok(id) = id_str.parse::<u64>() {
                            if id >= log_id {
                                batch.delete_cf(cf, key);
                            }
                        }
                    }
                }
            }
        }
        
        self.db.write(batch)
            .map_err(|e| StorageError::from_io_error(
                openraft::ErrorSubject::Logs,
                openraft::ErrorVerb::Delete,
                std::io::Error::new(std::io::ErrorKind::Other, e),
            ))?;
        
        Ok(())
    }
}

#[async_trait]
impl RaftLogReader<TypeConfig> for BrokerRaftStorage {
    async fn try_get_log_entries<RB: RangeBounds<u64> + Clone + Debug + Send + Sync>(
        &mut self,
        range: RB,
    ) -> Result<Vec<Entry<TypeConfig>>, StorageError<BrokerNodeId>> {
        let mut entries = Vec::new();
        
        // Convert range to concrete bounds
        let start = match range.start_bound() {
            std::ops::Bound::Included(&n) => n,
            std::ops::Bound::Excluded(&n) => n + 1,
            std::ops::Bound::Unbounded => 0,
        };
        
        let end = match range.end_bound() {
            std::ops::Bound::Included(&n) => n + 1,
            std::ops::Bound::Excluded(&n) => n,
            std::ops::Bound::Unbounded => u64::MAX,
        };
        
        for idx in start..end {
            if let Some(entry) = self.get_log(idx).await {
                entries.push(entry);
            } else {
                break;
            }
        }
        
        Ok(entries)
    }
}

#[async_trait]
impl RaftStorage<TypeConfig> for BrokerRaftStorage {
    type LogReader = Self;
    type SnapshotBuilder = BrokerStateMachine;

    async fn get_log_state(&mut self) -> Result<LogState<TypeConfig>, StorageError<BrokerNodeId>> {
        Ok(self.log_state.read().await.clone())
    }

    async fn save_vote(&mut self, vote: &Vote<BrokerNodeId>) -> Result<(), StorageError<BrokerNodeId>> {
        let value = bincode::serialize(vote)
            .map_err(|e| StorageError::from_io_error(
                openraft::ErrorSubject::Vote,
                openraft::ErrorVerb::Write,
                std::io::Error::new(std::io::ErrorKind::InvalidData, e),
            ))?;
        
        self.db.put_cf(self.get_cf(CF_STATE), b"vote", value)
            .map_err(|e| StorageError::from_io_error(
                openraft::ErrorSubject::Vote,
                openraft::ErrorVerb::Write,
                std::io::Error::new(std::io::ErrorKind::Other, e),
            ))?;
        
        *self.vote.write().await = Some(*vote);
        Ok(())
    }

    async fn read_vote(&mut self) -> Result<Option<Vote<BrokerNodeId>>, StorageError<BrokerNodeId>> {
        Ok(self.vote.read().await.clone())
    }

    async fn append_to_log<I>(&mut self, entries: I) -> Result<(), StorageError<BrokerNodeId>>
    where
        I: IntoIterator<Item = Entry<TypeConfig>> + Send,
    {
        let mut log_state = self.log_state.write().await;
        
        for entry in entries {
            // Save to RocksDB
            self.save_log(&entry).await?;
            
            // Update log state
            if log_state.last.is_none() || log_state.last.as_ref().unwrap() < &entry.log_id {
                log_state.last = Some(entry.log_id);
            }
        }
        
        Ok(())
    }

    async fn delete_conflict_logs_since(&mut self, log_id: LogId<BrokerNodeId>) -> Result<(), StorageError<BrokerNodeId>> {
        self.delete_logs_from(log_id.index).await?;
        
        // Update log state
        let mut log_state = self.log_state.write().await;
        if log_id.index > 0 {
            if let Some(entry) = self.get_log(log_id.index - 1).await {
                log_state.last = Some(entry.log_id);
            } else {
                log_state.last = None;
            }
        } else {
            log_state.last = None;
        }
        
        Ok(())
    }

    async fn purge_logs_upto(&mut self, log_id: LogId<BrokerNodeId>) -> Result<(), StorageError<BrokerNodeId>> {
        // Delete logs up to and including log_id
        let cf = self.get_cf(CF_LOGS);
        let mut batch = rocksdb::WriteBatch::default();
        
        for idx in 0..=log_id.index {
            let key = format!("log_{:020}", idx);
            batch.delete_cf(cf, key.as_bytes());
        }
        
        self.db.write(batch)
            .map_err(|e| StorageError::from_io_error(
                openraft::ErrorSubject::Log(log_id),
                openraft::ErrorVerb::Delete,
                std::io::Error::new(std::io::ErrorKind::Other, e),
            ))?;
        
        Ok(())
    }

    async fn last_applied_state(&mut self) -> Result<(Option<LogId<BrokerNodeId>>, StoredMembership<BrokerNodeId, BrokerNode>), StorageError<BrokerNodeId>> {
        self.state_machine.write().await.applied_state().await
    }

    async fn apply_to_state_machine(&mut self, entries: &[Entry<TypeConfig>]) -> Result<Vec<crate::broker::raft_types::BrokerResponse>, StorageError<BrokerNodeId>> {
        self.state_machine.write().await.apply(entries.to_vec()).await
    }

    async fn get_log_reader(&mut self) -> Self::LogReader {
        self.clone()
    }

    async fn get_snapshot_builder(&mut self) -> Self::SnapshotBuilder {
        self.state_machine.write().await.get_snapshot_builder().await
    }

    async fn begin_receiving_snapshot(&mut self) -> Result<Box<std::io::Cursor<Vec<u8>>>, StorageError<BrokerNodeId>> {
        self.state_machine.write().await.begin_receiving_snapshot().await
    }

    async fn install_snapshot(
        &mut self,
        meta: &SnapshotMeta<BrokerNodeId, BrokerNode>,
        snapshot: Box<std::io::Cursor<Vec<u8>>>,
    ) -> Result<(), StorageError<BrokerNodeId>> {
        // Install to state machine
        self.state_machine.write().await.install_snapshot(meta, snapshot).await?;
        
        // Save snapshot metadata
        let snapshot_data = StorageSnapshot {
            meta: meta.clone(),
            data: Vec::new(), // We don't store snapshot data in storage for now
        };
        
        *self.current_snapshot.write().await = Some(snapshot_data);
        Ok(())
    }

    async fn get_current_snapshot(&mut self) -> Result<Option<openraft::storage::Snapshot<TypeConfig>>, StorageError<BrokerNodeId>> {
        self.state_machine.write().await.get_current_snapshot().await
    }
}

impl Clone for BrokerRaftStorage {
    fn clone(&self) -> Self {
        Self {
            db: self.db.clone(),
            state_machine: self.state_machine.clone(),
            vote: self.vote.clone(),
            log_state: self.log_state.clone(),
            current_snapshot: self.current_snapshot.clone(),
        }
    }
}
