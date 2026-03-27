use anyhow::Result;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio::time::{interval, Duration};
use tonic::Request;

use crate::api::broker::*;
use crate::client::config::ProducerConfig;
use crate::client::kafka_broker_client::KafkaBrokerClientTrait;
use crate::client::metadata_cache::MetadataCache;

/// Message to be sent by the producer
#[derive(Debug, Clone)]
pub struct ProducerMessage {
    pub key: Option<Vec<u8>>,
    pub value: Vec<u8>,
    pub partition: Option<i32>,
}

impl ProducerMessage {
    pub fn new(value: impl Into<Vec<u8>>) -> Self {
        Self {
            key: None,
            value: value.into(),
            partition: None,
        }
    }

    pub fn with_key(key: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>) -> Self {
        Self {
            key: Some(key.into()),
            value: value.into(),
            partition: None,
        }
    }

    pub fn to_partition(mut self, partition: i32) -> Self {
        self.partition = Some(partition);
        self
    }
}

/// Producer result containing offset information
#[derive(Debug, Clone)]
pub struct ProducerResult {
    pub partition: i32,
    pub offset: i64,
    pub error_code: i32,
}

/// Kafka-compatible producer with metadata-cache based leader routing.
///
/// On startup the producer connects to the bootstrap broker.  The first time a
/// partition is written to, it fetches `TopicMetadata` to find the partition
/// leader and routes the produce request directly to that broker.  On
/// `NOT_LEADER` (error_code 6) or a transport error, it invalidates the cached
/// leader entry, re-fetches metadata, and retries — up to 3 attempts with an
/// exponential back-off — so that a leader failover is handled transparently.
pub struct Producer {
    config: ProducerConfig,
    meta: Arc<Mutex<MetadataCache>>,
    batch: Arc<Mutex<Vec<ProducerMessage>>>,
    shutdown_tx: mpsc::Sender<()>,
    round_robin_counter: Arc<AtomicU64>,
}

impl Producer {
    /// Create a new producer and start the background flush task.
    pub async fn new(broker_address: &str, config: ProducerConfig) -> Result<Self> {
        let meta = Arc::new(Mutex::new(MetadataCache::new(broker_address)));
        let batch = Arc::new(Mutex::new(Vec::new()));
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
        let round_robin_counter = Arc::new(AtomicU64::new(0));

        {
            let batch = batch.clone();
            let meta = meta.clone();
            let config = config.clone();
            let counter = round_robin_counter.clone();
            tokio::spawn(async move {
                let mut flush_interval = interval(Duration::from_millis(config.flush_interval_ms));
                loop {
                    tokio::select! {
                        _ = flush_interval.tick() => {
                            let messages = {
                                let mut lock = batch.lock().await;
                                if lock.is_empty() { continue; }
                                std::mem::take(&mut *lock)
                            };
                            if let Err(e) = Self::send_batch_inner(&meta, &config, messages, &counter).await {
                                log::error!("Failed to flush batch: {}", e);
                            }
                        }
                        _ = shutdown_rx.recv() => {
                            log::info!("Producer flush task shutting down");
                            let messages = std::mem::take(&mut *batch.lock().await);
                            if !messages.is_empty() {
                                if let Err(e) = Self::send_batch_inner(&meta, &config, messages, &counter).await {
                                    log::error!("Failed to flush final batch: {}", e);
                                }
                            }
                            break;
                        }
                    }
                }
            });
        }

        log::info!("Producer started for topic: {}", config.topic);
        Ok(Self {
            config,
            meta,
            batch,
            shutdown_tx,
            round_robin_counter,
        })
    }

    /// Send a single message (adds to batch, flushes immediately when batch is full).
    pub async fn send(&self, message: ProducerMessage) -> Result<()> {
        let mut batch = self.batch.lock().await;
        batch.push(message);
        if batch.len() >= self.config.batch_size {
            let messages = std::mem::take(&mut *batch);
            drop(batch);
            Self::send_batch_inner(
                &self.meta,
                &self.config,
                messages,
                &self.round_robin_counter,
            )
            .await?;
        }
        Ok(())
    }

    /// Send a message synchronously and wait for the broker acknowledgement.
    pub async fn send_sync(&self, message: ProducerMessage) -> Result<ProducerResult> {
        let partition =
            Self::assign_partition(&message, &self.config, &self.round_robin_counter);
        let records = vec![produce_request::PartitionData {
            partition,
            records: vec![Record {
                key: message.key,
                value: message.value,
            }],
        }];
        let req = ProduceRequest {
            required_acks: self.config.required_acks,
            timeout_ms: self.config.timeout_ms,
            topics: vec![produce_request::TopicData {
                topic_name: self.config.topic.clone(),
                partitions: records,
            }],
        };

        // Resolve leader and send with retry.
        const MAX_ATTEMPTS: u32 = 3;
        for attempt in 0..MAX_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(100 * attempt as u64)).await;
            }

            // Resolve leader (hold lock briefly, release before RPC).
            let client = {
                let mut cache = self.meta.lock().await;
                let addr = match cache.get_leader(&self.config.topic, partition) {
                    Some(a) => a.to_string(),
                    None => {
                        cache.refresh(&self.config.topic).await?;
                        cache
                            .get_leader(&self.config.topic, partition)
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "No leader for {}-{}",
                                    self.config.topic,
                                    partition
                                )
                            })?
                            .to_string()
                    }
                };
                cache.get_or_connect(&addr).await?
            };

            match client.produce(Request::new(req.clone())).await {
                Ok(response) => {
                    for topic_result in response.results {
                        for pr in topic_result.partitions {
                            if pr.error_code == 0 {
                                return Ok(ProducerResult {
                                    partition: pr.partition,
                                    offset: pr.offset,
                                    error_code: 0,
                                });
                            } else if pr.error_code == 6 {
                                let mut cache = self.meta.lock().await;
                                cache.invalidate_topic(&self.config.topic);
                            } else {
                                anyhow::bail!(
                                    "Produce failed for partition {}: error_code={}",
                                    pr.partition,
                                    pr.error_code
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    log::warn!("Attempt {}/{}: transport error: {}", attempt + 1, MAX_ATTEMPTS, e);
                    let mut cache = self.meta.lock().await;
                    cache.invalidate_topic(&self.config.topic);
                }
            }
        }
        anyhow::bail!(
            "Produce failed after {} attempts (NOT_LEADER or transport error)",
            MAX_ATTEMPTS
        )
    }

    /// Flush any pending messages immediately.
    pub async fn flush(&self) -> Result<()> {
        let messages = std::mem::take(&mut *self.batch.lock().await);
        if !messages.is_empty() {
            Self::send_batch_inner(
                &self.meta,
                &self.config,
                messages,
                &self.round_robin_counter,
            )
            .await?;
        }
        Ok(())
    }

    fn assign_partition(
        msg: &ProducerMessage,
        config: &ProducerConfig,
        counter: &Arc<AtomicU64>,
    ) -> i32 {
        if let Some(p) = msg.partition {
            return p;
        }
        match config.partitioning.as_str() {
            "round_robin" => {
                let next = counter.fetch_add(1, Ordering::Relaxed);
                (next % config.num_partitions.max(1) as u64) as i32
            }
            "key_hash" => {
                if let Some(key) = &msg.key {
                    let mut hasher = DefaultHasher::new();
                    key.hash(&mut hasher);
                    (hasher.finish() % config.num_partitions.max(1) as u64) as i32
                } else {
                    config.partition
                }
            }
            _ => config.partition,
        }
    }

    /// Core send path.
    ///
    /// 1. Assigns each message to a partition.
    /// 2. Resolves the leader broker per partition via the metadata cache.
    /// 3. Groups partitions by broker and sends one `ProduceRequest` per broker.
    /// 4. On `NOT_LEADER` (error_code 6) or transport error: invalidates the
    ///    topic's cache entries, waits briefly, and retries — up to 3 attempts.
    async fn send_batch_inner(
        meta: &Arc<Mutex<MetadataCache>>,
        config: &ProducerConfig,
        messages: Vec<ProducerMessage>,
        counter: &Arc<AtomicU64>,
    ) -> Result<()> {
        if messages.is_empty() {
            return Ok(());
        }

        // Build per-partition record lists (done once, reused on retries).
        let mut records_by_partition: std::collections::HashMap<i32, Vec<Record>> =
            std::collections::HashMap::new();
        for msg in messages {
            let partition = Self::assign_partition(&msg, config, counter);
            records_by_partition
                .entry(partition)
                .or_default()
                .push(Record { key: msg.key, value: msg.value });
        }
        let all_partitions: Vec<i32> = records_by_partition.keys().copied().collect();

        const MAX_ATTEMPTS: u32 = 3;
        for attempt in 0..MAX_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(100 * attempt as u64)).await;
            }

            // ── Phase 1: resolve leaders (Mutex held, may do one refresh RPC) ──
            let (by_broker, clients) = {
                let mut cache = meta.lock().await;
                let by_broker = cache
                    .resolve_leaders(&config.topic, &all_partitions)
                    .await?;
                // Pre-connect to every leader while we hold the lock.
                let mut clients = std::collections::HashMap::new();
                for addr in by_broker.keys() {
                    clients.insert(addr.clone(), cache.get_or_connect(addr).await?);
                }
                (by_broker, clients)
            }; // ── Mutex released before RPCs ──

            // ── Phase 2: send one ProduceRequest per broker (no lock held) ──
            let mut not_leader = false;
            let mut hard_failures: Vec<(i32, i32)> = Vec::new(); // (partition, error_code)

            for (broker_addr, partitions) in &by_broker {
                let partition_data: Vec<produce_request::PartitionData> = partitions
                    .iter()
                    .map(|p| produce_request::PartitionData {
                        partition: *p,
                        records: records_by_partition[p].clone(),
                    })
                    .collect();

                let req = ProduceRequest {
                    required_acks: config.required_acks,
                    timeout_ms: config.timeout_ms,
                    topics: vec![produce_request::TopicData {
                        topic_name: config.topic.clone(),
                        partitions: partition_data,
                    }],
                };

                let client = clients[broker_addr].clone();
                match client.produce(Request::new(req)).await {
                    Err(e) => {
                        log::warn!(
                            "Attempt {}/{}: transport error to {}: {}",
                            attempt + 1,
                            MAX_ATTEMPTS,
                            broker_addr,
                            e
                        );
                        not_leader = true; // treat as stale metadata, refresh and retry
                    }
                    Ok(response) => {
                        for topic_result in &response.results {
                            for pr in &topic_result.partitions {
                                if pr.error_code == 6 {
                                    log::info!(
                                        "Attempt {}/{}: NOT_LEADER for partition {}",
                                        attempt + 1,
                                        MAX_ATTEMPTS,
                                        pr.partition
                                    );
                                    not_leader = true;
                                } else if pr.error_code != 0 {
                                    log::warn!(
                                        "Produce failed for partition {}: error_code={}",
                                        pr.partition,
                                        pr.error_code
                                    );
                                    hard_failures.push((pr.partition, pr.error_code));
                                } else {
                                    log::debug!(
                                        "Sent partition={} offset={}",
                                        pr.partition,
                                        pr.offset
                                    );
                                }
                            }
                        }
                    }
                }
            }

            if !hard_failures.is_empty() {
                anyhow::bail!("Produce hard-failed for partitions: {:?}", hard_failures);
            }

            if !not_leader {
                return Ok(()); // all partitions succeeded
            }

            // ── Phase 3: invalidate stale metadata, loop will refresh ──
            {
                let mut cache = meta.lock().await;
                cache.invalidate_topic(&config.topic);
            }
        }

        anyhow::bail!(
            "Produce failed after {} attempts (NOT_LEADER or transport errors)",
            MAX_ATTEMPTS
        )
    }

    /// Shutdown the producer gracefully.
    pub async fn shutdown(self) -> Result<()> {
        log::info!("Shutting down producer...");
        self.flush().await?;
        let _ = self.shutdown_tx.send(()).await;
        log::info!("Producer shutdown complete");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::config::ProducerConfig;
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;

    #[test]
    fn test_producer_message_creation() {
        let msg = ProducerMessage::new(b"test");
        assert_eq!(msg.value, b"test");
        assert!(msg.key.is_none());

        let msg_with_key = ProducerMessage::with_key(b"key", b"value");
        assert_eq!(msg_with_key.key.unwrap(), b"key");
        assert_eq!(msg_with_key.value, b"value");

        let msg_with_partition = ProducerMessage::new(b"test").to_partition(1);
        assert_eq!(msg_with_partition.partition, Some(1));
    }

    #[test]
    fn test_assign_partition_round_robin() {
        let config = ProducerConfig {
            topic: "test".to_string(),
            partition: 0,
            partitioning: "round_robin".to_string(),
            num_partitions: 3,
            required_acks: 1,
            timeout_ms: 5000,
            batch_size: 100,
            flush_interval_ms: 100,
        };
        let counter = Arc::new(AtomicU64::new(0));
        let msg = ProducerMessage::new(b"v");

        assert_eq!(Producer::assign_partition(&msg, &config, &counter), 0);
        assert_eq!(Producer::assign_partition(&msg, &config, &counter), 1);
        assert_eq!(Producer::assign_partition(&msg, &config, &counter), 2);
        assert_eq!(Producer::assign_partition(&msg, &config, &counter), 0);
    }

    #[test]
    fn test_assign_partition_key_hash_is_stable_for_same_key() {
        let config = ProducerConfig {
            topic: "test".to_string(),
            partition: 0,
            partitioning: "key_hash".to_string(),
            num_partitions: 8,
            required_acks: 1,
            timeout_ms: 5000,
            batch_size: 100,
            flush_interval_ms: 100,
        };
        let counter = Arc::new(AtomicU64::new(0));
        let m1 = ProducerMessage::with_key(b"order-42", b"v1");
        let m2 = ProducerMessage::with_key(b"order-42", b"v2");

        let p1 = Producer::assign_partition(&m1, &config, &counter);
        let p2 = Producer::assign_partition(&m2, &config, &counter);
        assert_eq!(p1, p2);
        assert!(p1 >= 0 && p1 < config.num_partitions);
    }

    #[test]
    fn test_assign_partition_round_robin_near_u64_max() {
        let config = ProducerConfig {
            topic: "test".to_string(),
            partition: 0,
            partitioning: "round_robin".to_string(),
            num_partitions: 3,
            required_acks: 1,
            timeout_ms: 5000,
            batch_size: 100,
            flush_interval_ms: 100,
        };
        let counter = Arc::new(AtomicU64::new(u64::MAX - 1));
        let msg = ProducerMessage::new(b"v");
        let p1 = Producer::assign_partition(&msg, &config, &counter);
        let p2 = Producer::assign_partition(&msg, &config, &counter);
        assert!(p1 >= 0 && p1 < 3);
        assert!(p2 >= 0 && p2 < 3);
    }

    #[test]
    fn test_assign_partition_key_hash_falls_back_to_fixed_without_key() {
        let config = ProducerConfig {
            topic: "test".to_string(),
            partition: 2,
            partitioning: "key_hash".to_string(),
            num_partitions: 8,
            required_acks: 1,
            timeout_ms: 5000,
            batch_size: 100,
            flush_interval_ms: 100,
        };
        let counter = Arc::new(AtomicU64::new(0));
        let msg = ProducerMessage::new(b"no-key");
        let p = Producer::assign_partition(&msg, &config, &counter);
        assert_eq!(p, 2);
    }
}
