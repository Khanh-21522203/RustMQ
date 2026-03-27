use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::watch;

use crate::broker::storage::StoredMessage;

/// A single log entry stored in a partition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub offset: i64,
    pub key: Option<Vec<u8>>,
    pub value: Vec<u8>,
    pub timestamp_ms: i64,
}

impl From<&LogEntry> for StoredMessage {
    fn from(e: &LogEntry) -> Self {
        StoredMessage {
            offset: e.offset,
            key: e.key.clone(),
            value: e.value.clone(),
            timestamp_ms: e.timestamp_ms,
        }
    }
}

/// Append-only log for a single (topic, partition), backed by a sled tree.
///
/// # Offset semantics
/// - **LEO** (Log End Offset): the next offset to be assigned. After appending
///   offset N, LEO = N + 1.
/// - **HW** (High Watermark): the highest offset that is fully replicated across
///   all in-sync replicas, expressed as *next offset* (i.e. HW = last committed
///   offset + 1). Consumers only read entries with offset < HW.
///
/// Both values are kept in `AtomicI64` so they can be read without locking.
///
/// # Sled key layout
/// - Message entries: 8-byte big-endian offset → bincode(LogEntry)
/// - Persisted HW:    b"__hw__" → bincode(i64)
///
/// The `__hw__` key sorts after all 8-byte offset keys (first byte `_`=95 vs 0
/// for any reasonable offset), so range queries over offset space are safe.
pub struct PartitionLog {
    tree: sled::Tree,
    leo: AtomicI64,
    hw: AtomicI64,
    /// Notifies waiters whenever the high-watermark advances.
    hw_tx: watch::Sender<i64>,
}

impl PartitionLog {
    /// Open (or create) a `PartitionLog` backed by `tree`.
    /// Recovers LEO and HW from previously persisted data.
    pub fn open(tree: sled::Tree) -> anyhow::Result<Self> {
        // Recover LEO: find the last 8-byte offset key, ignoring meta keys such
        // as b"__hw__" (6 bytes, first byte `_`=95 which falls inside the i64 range).
        let leo = {
            let range = offset_to_key(0)..=offset_to_key(i64::MAX);
            let mut found = 0i64;
            for item in tree.range(range).rev() {
                if let Ok((key, _)) = item {
                    if key.len() == 8 {
                        found = key_to_offset(&key)? + 1;
                        break;
                    }
                }
            }
            found
        };

        // Recover HW from persisted meta key.
        let hw = tree
            .get(b"__hw__")?
            .and_then(|v| bincode::deserialize::<i64>(&v).ok())
            .unwrap_or(0);

        let (hw_tx, _) = watch::channel(hw);
        Ok(Self {
            tree,
            leo: AtomicI64::new(leo),
            hw: AtomicI64::new(hw),
            hw_tx,
        })
    }

    /// Append a message to the log. Returns the assigned offset.
    pub fn append(&self, key: Option<Vec<u8>>, value: Vec<u8>) -> anyhow::Result<i64> {
        let offset = self.leo.fetch_add(1, Ordering::SeqCst);
        let entry = LogEntry {
            offset,
            key,
            value,
            timestamp_ms: now_ms(),
        };
        let sled_key = offset_to_key(offset);
        let bytes = bincode::serialize(&entry)?;
        self.tree.insert(sled_key, bytes)?;
        Ok(offset)
    }

    /// Read messages starting at `start_offset`, up to `max_bytes` total payload.
    /// Only returns entries with offset < `high_watermark()`.
    pub fn read(&self, start_offset: i64, max_bytes: i32) -> anyhow::Result<Vec<StoredMessage>> {
        let hw = self.hw.load(Ordering::SeqCst);
        let mut result = Vec::new();
        let mut total_bytes = 0usize;

        // Range is [start_offset, hw) — exclusive upper bound keeps entries below HW.
        let range = offset_to_key(start_offset)..offset_to_key(hw);
        for item in self.tree.range(range) {
            let (_, val) = item?;
            let entry: LogEntry = bincode::deserialize(&val)?;
            let size = entry.value.len() + entry.key.as_ref().map_or(0, |k| k.len());
            if total_bytes + size > max_bytes as usize && !result.is_empty() {
                break;
            }
            result.push(StoredMessage::from(&entry));
            total_bytes += size;
        }
        Ok(result)
    }

    /// Returns the next offset to be written (LEO).
    pub fn log_end_offset(&self) -> i64 {
        self.leo.load(Ordering::SeqCst)
    }

    /// Returns the high watermark (= last committed offset + 1).
    /// Consumers should read only up to this value.
    pub fn high_watermark(&self) -> i64 {
        self.hw.load(Ordering::SeqCst)
    }

    /// Advance the high watermark to `new_hw`. Monotonic — never goes backwards.
    /// Persists the new value to sled so it survives restarts, and notifies
    /// any `acks=all` waiters subscribed via [`subscribe_hw`].
    pub fn advance_hw(&self, new_hw: i64) -> anyhow::Result<()> {
        let prev = self.hw.fetch_max(new_hw, Ordering::SeqCst);
        if new_hw > prev {
            let bytes = bincode::serialize(&new_hw)?;
            self.tree.insert(b"__hw__", bytes)?;
            let _ = self.hw_tx.send(new_hw);
        }
        Ok(())
    }

    /// Subscribe to high-watermark advances. Returns a [`watch::Receiver`] whose
    /// value is always the latest HW; call `.changed().await` to wait for the
    /// next advance.
    pub fn subscribe_hw(&self) -> watch::Receiver<i64> {
        self.hw_tx.subscribe()
    }

    /// Earliest available offset, or `None` if the log is empty.
    pub fn first_offset(&self) -> anyhow::Result<Option<i64>> {
        let range = offset_to_key(0)..=offset_to_key(i64::MAX);
        for item in self.tree.range(range) {
            let (key, _) = item?;
            if key.len() == 8 {
                return Ok(Some(key_to_offset(&key)?));
            }
        }
        Ok(None)
    }

    /// Remove all entries with offset < `keep_from_offset` (retention / log compaction).
    /// Does not change LEO or HW — callers are responsible for correctness.
    pub fn truncate_before(&self, keep_from_offset: i64) -> anyhow::Result<()> {
        let end_key = offset_to_key(keep_from_offset);
        for item in self.tree.range(..end_key) {
            let (key, _) = item?;
            self.tree.remove(key)?;
        }
        Ok(())
    }
}

/// Encode an offset as an 8-byte big-endian key so sled iterates in order.
fn offset_to_key(offset: i64) -> [u8; 8] {
    offset.to_be_bytes()
}

fn key_to_offset(key: &[u8]) -> anyhow::Result<i64> {
    let arr: [u8; 8] = key
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid key length {}", key.len()))?;
    Ok(i64::from_be_bytes(arr))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_log() -> PartitionLog {
        let db = sled::Config::new().temporary(true).open().unwrap();
        let tree = db.open_tree("test").unwrap();
        PartitionLog::open(tree).unwrap()
    }

    #[test]
    fn append_assigns_sequential_offsets() {
        let log = temp_log();
        assert_eq!(log.append(None, b"a".to_vec()).unwrap(), 0);
        assert_eq!(log.append(None, b"b".to_vec()).unwrap(), 1);
        assert_eq!(log.append(None, b"c".to_vec()).unwrap(), 2);
        assert_eq!(log.log_end_offset(), 3);
    }

    #[test]
    fn read_respects_high_watermark() {
        let log = temp_log();
        log.append(None, b"m0".to_vec()).unwrap();
        log.append(None, b"m1".to_vec()).unwrap();
        log.append(None, b"m2".to_vec()).unwrap();

        // HW = 0: nothing visible yet
        assert!(log.read(0, 1024).unwrap().is_empty());

        // Advance HW to 2: offsets 0 and 1 are visible
        log.advance_hw(2).unwrap();
        let msgs = log.read(0, 1024).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].offset, 0);
        assert_eq!(msgs[1].offset, 1);

        // Read starting at offset 1: only offset 1
        let msgs = log.read(1, 1024).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].offset, 1);
    }

    #[test]
    fn hw_only_advances_forward() {
        let log = temp_log();
        log.append(None, b"x".to_vec()).unwrap();
        log.advance_hw(1).unwrap();
        assert_eq!(log.high_watermark(), 1);
        log.advance_hw(0).unwrap(); // attempt to move backwards
        assert_eq!(log.high_watermark(), 1); // unchanged
    }

    #[test]
    fn truncate_before_removes_old_entries() {
        let log = temp_log();
        for i in 0..5 {
            log.append(None, format!("m{i}").into_bytes()).unwrap();
        }
        log.advance_hw(5).unwrap();
        log.truncate_before(3).unwrap();

        let msgs = log.read(0, 10240).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].offset, 3);
        assert_eq!(msgs[1].offset, 4);
    }

    #[test]
    fn leo_and_hw_recovered_after_reopen() {
        let db = sled::Config::new().temporary(true).open().unwrap();
        {
            let tree = db.open_tree("p0").unwrap();
            let log = PartitionLog::open(tree).unwrap();
            log.append(None, b"a".to_vec()).unwrap();
            log.append(None, b"b".to_vec()).unwrap();
            log.advance_hw(2).unwrap();
        }
        // Reopen the same tree — LEO and HW must be restored
        let tree = db.open_tree("p0").unwrap();
        let log = PartitionLog::open(tree).unwrap();
        assert_eq!(log.log_end_offset(), 2);
        assert_eq!(log.high_watermark(), 2);
    }

    #[test]
    fn read_max_bytes_limits_results() {
        let log = temp_log();
        // Three messages of 100 bytes each
        for i in 0..3 {
            log.append(None, vec![i as u8; 100]).unwrap();
        }
        log.advance_hw(3).unwrap();
        // 150-byte budget: first message (100 bytes) fits; second would exceed
        let msgs = log.read(0, 150).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].offset, 0);
    }

    #[test]
    fn read_empty_range_returns_empty() {
        let log = temp_log();
        log.append(None, b"x".to_vec()).unwrap();
        // HW still 0 — nothing committed
        assert!(log.read(0, 1024).unwrap().is_empty());
    }

    #[test]
    fn append_preserves_key_and_value() {
        let log = temp_log();
        let key = Some(b"my-key".to_vec());
        let value = b"my-value".to_vec();
        log.append(key.clone(), value.clone()).unwrap();
        log.advance_hw(1).unwrap();
        let msgs = log.read(0, 1024).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].key, key);
        assert_eq!(msgs[0].value, value);
    }
}
