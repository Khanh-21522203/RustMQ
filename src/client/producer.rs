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
use crate::client::kafka_broker_client::{KafkaBrokerClient, KafkaBrokerClientTrait};

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

/// Kafka Producer
pub struct Producer {
    config: ProducerConfig,
    /// Mutable client so that NOT_LEADER redirects can update the target broker.
    client: Arc<Mutex<Arc<KafkaBrokerClient>>>,
    batch: Arc<Mutex<Vec<ProducerMessage>>>,
    shutdown_tx: mpsc::Sender<()>,
    /// Counter for round-robin partition assignment
    round_robin_counter: Arc<AtomicU64>,
}

impl Producer {
    /// Create a new producer and start the background flush task.
    pub async fn new(broker_address: &str, config: ProducerConfig) -> Result<Self> {
        let inner_client = Arc::new(
            KafkaBrokerClient::new(broker_address)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to connect to broker: {}", e))?,
        );
        let client = Arc::new(Mutex::new(inner_client));
        let batch = Arc::new(Mutex::new(Vec::new()));
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

        let round_robin_counter = Arc::new(AtomicU64::new(0));

        // Start background flush task immediately
        {
            let batch = batch.clone();
            let client = client.clone();
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
                            if let Err(e) = Self::send_batch_inner(&client, &config, messages, &counter).await {
                                log::error!("Failed to flush batch: {}", e);
                            }
                        }
                        _ = shutdown_rx.recv() => {
                            log::info!("Producer flush task shutting down");
                            // Final flush
                            let messages = std::mem::take(&mut *batch.lock().await);
                            if !messages.is_empty() {
                                if let Err(e) = Self::send_batch_inner(&client, &config, messages, &counter).await {
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
            client,
            batch,
            shutdown_tx,
            round_robin_counter,
        })
    }

    /// Send a single message (adds to batch).
    pub async fn send(&self, message: ProducerMessage) -> Result<()> {
        let mut batch = self.batch.lock().await;
        batch.push(message);
        if batch.len() >= self.config.batch_size {
            let messages = std::mem::take(&mut *batch);
            drop(batch);
            Self::send_batch_inner(
                &self.client,
                &self.config,
                messages,
                &self.round_robin_counter,
            )
            .await?;
        }
        Ok(())
    }

    /// Send a message synchronously (bypasses batch).
    pub async fn send_sync(&self, message: ProducerMessage) -> Result<ProducerResult> {
        let partition = Self::assign_partition(&message, &self.config, &self.round_robin_counter);
        let request = Request::new(ProduceRequest {
            required_acks: self.config.required_acks,
            timeout_ms: self.config.timeout_ms,
            topics: vec![produce_request::TopicData {
                topic_name: self.config.topic.clone(),
                partitions: vec![produce_request::PartitionData {
                    partition,
                    records: vec![Record {
                        key: message.key,
                        value: message.value,
                    }],
                }],
            }],
        });

        let client = self.client.lock().await.clone();
        let response = client.produce(request).await?;
        for topic_result in response.results {
            for partition_result in topic_result.partitions {
                if partition_result.error_code == 0 {
                    return Ok(ProducerResult {
                        partition: partition_result.partition,
                        offset: partition_result.offset,
                        error_code: 0,
                    });
                } else {
                    anyhow::bail!(
                        "Failed to send message: error_code={}",
                        partition_result.error_code
                    );
                }
            }
        }
        anyhow::bail!("No response from broker")
    }

    /// Flush any pending messages immediately.
    pub async fn flush(&self) -> Result<()> {
        let messages = std::mem::take(&mut *self.batch.lock().await);
        if !messages.is_empty() {
            Self::send_batch_inner(
                &self.client,
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
            _ => config.partition, // "fixed" or unknown
        }
    }

    async fn send_batch_inner(
        client_holder: &Arc<Mutex<Arc<KafkaBrokerClient>>>,
        config: &ProducerConfig,
        messages: Vec<ProducerMessage>,
        counter: &Arc<AtomicU64>,
    ) -> Result<()> {
        if messages.is_empty() {
            return Ok(());
        }

        // Group messages by partition
        let mut by_partition: std::collections::HashMap<i32, Vec<Record>> =
            std::collections::HashMap::new();
        for msg in messages {
            let partition = Self::assign_partition(&msg, config, counter);
            by_partition.entry(partition).or_default().push(Record {
                key: msg.key,
                value: msg.value,
            });
        }

        let partitions: Vec<produce_request::PartitionData> = by_partition
            .into_iter()
            .map(|(partition, records)| produce_request::PartitionData { partition, records })
            .collect();

        let produce_req = ProduceRequest {
            required_acks: config.required_acks,
            timeout_ms: config.timeout_ms,
            topics: vec![produce_request::TopicData {
                topic_name: config.topic.clone(),
                partitions,
            }],
        };

        let client = client_holder.lock().await.clone();
        let response = client.produce(Request::new(produce_req.clone())).await?;

        // Check if any partition returned NOT_LEADER (error_code=6) with a redirect address.
        let mut leader_redirect: Option<String> = None;
        let mut failed_partitions = Vec::new();
        for topic_result in &response.results {
            for partition_result in &topic_result.partitions {
                if partition_result.error_code == 6 && !partition_result.leader_addr.is_empty() {
                    leader_redirect = Some(partition_result.leader_addr.clone());
                } else if partition_result.error_code != 0 {
                    log::warn!(
                        "Failed to send to partition {}: error_code={}",
                        partition_result.partition,
                        partition_result.error_code
                    );
                    failed_partitions.push(partition_result.partition);
                } else {
                    log::debug!(
                        "Batch sent: partition={}, offset={}",
                        partition_result.partition,
                        partition_result.offset
                    );
                }
            }
        }

        if let Some(leader_addr) = leader_redirect {
            log::info!("NOT_LEADER — redirecting to leader at {}", leader_addr);
            let new_client = Arc::new(
                KafkaBrokerClient::new(&leader_addr)
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to connect to leader {}: {}", leader_addr, e))?,
            );
            *client_holder.lock().await = new_client.clone();

            // Retry once against the leader.
            let retry_response = new_client.produce(Request::new(produce_req)).await?;
            for topic_result in &retry_response.results {
                for partition_result in &topic_result.partitions {
                    if partition_result.error_code != 0 {
                        anyhow::bail!(
                            "Produce to leader failed for partition {}: error_code={}",
                            partition_result.partition,
                            partition_result.error_code
                        );
                    } else {
                        log::debug!(
                            "Batch sent (after redirect): partition={}, offset={}",
                            partition_result.partition,
                            partition_result.offset
                        );
                    }
                }
            }
            return Ok(());
        }

        if !failed_partitions.is_empty() {
            anyhow::bail!("Produce failed for partitions: {:?}", failed_partitions);
        }
        Ok(())
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
        // Start near u64::MAX to verify wrap-around does not panic
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
        let msg = ProducerMessage::new(b"no-key"); // no key set
        let p = Producer::assign_partition(&msg, &config, &counter);
        // Should fall back to config.partition (2) when no key is present
        assert_eq!(p, 2);
    }
}
