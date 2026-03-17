use anyhow::Result;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
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
        Self { key: None, value: value.into(), partition: None }
    }

    pub fn with_key(key: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>) -> Self {
        Self { key: Some(key.into()), value: value.into(), partition: None }
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
    client: Arc<KafkaBrokerClient>,
    batch: Arc<Mutex<Vec<ProducerMessage>>>,
    shutdown_tx: mpsc::Sender<()>,
    /// Counter for round-robin partition assignment
    round_robin_counter: Arc<AtomicU32>,
}

impl Producer {
    /// Create a new producer and start the background flush task.
    pub async fn new(broker_address: &str, config: ProducerConfig) -> Result<Self> {
        let client = Arc::new(
            KafkaBrokerClient::new(broker_address)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to connect to broker: {}", e))?,
        );
        let batch = Arc::new(Mutex::new(Vec::new()));
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

        let round_robin_counter = Arc::new(AtomicU32::new(0));

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
        Ok(Self { config, client, batch, shutdown_tx, round_robin_counter })
    }

    /// Send a single message (adds to batch).
    pub async fn send(&self, message: ProducerMessage) -> Result<()> {
        let mut batch = self.batch.lock().await;
        batch.push(message);
        if batch.len() >= self.config.batch_size {
            let messages = std::mem::take(&mut *batch);
            drop(batch);
            Self::send_batch_inner(&self.client, &self.config, messages, &self.round_robin_counter).await?;
        }
        Ok(())
    }

    /// Send a message synchronously (bypasses batch).
    pub async fn send_sync(&self, message: ProducerMessage) -> Result<ProducerResult> {
        let partition = message.partition.unwrap_or(self.config.partition);
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

        let response = self.client.produce(request).await?;
        for topic_result in response.results {
            for partition_result in topic_result.partitions {
                if partition_result.error_code == 0 {
                    return Ok(ProducerResult {
                        partition: partition_result.partition,
                        offset: partition_result.offset,
                        error_code: 0,
                    });
                } else {
                    anyhow::bail!("Failed to send message: error_code={}", partition_result.error_code);
                }
            }
        }
        anyhow::bail!("No response from broker")
    }

    /// Flush any pending messages immediately.
    pub async fn flush(&self) -> Result<()> {
        let messages = std::mem::take(&mut *self.batch.lock().await);
        if !messages.is_empty() {
            Self::send_batch_inner(&self.client, &self.config, messages, &self.round_robin_counter).await?;
        }
        Ok(())
    }

    fn assign_partition(msg: &ProducerMessage, config: &ProducerConfig, counter: &Arc<AtomicU32>) -> i32 {
        if let Some(p) = msg.partition {
            return p;
        }
        match config.partitioning.as_str() {
            "round_robin" => {
                let next = counter.fetch_add(1, Ordering::Relaxed);
                (next % config.num_partitions.max(1) as u32) as i32
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
        client: &KafkaBrokerClient,
        config: &ProducerConfig,
        messages: Vec<ProducerMessage>,
        counter: &Arc<AtomicU32>,
    ) -> Result<()> {
        if messages.is_empty() {
            return Ok(());
        }

        // Group messages by partition
        let mut by_partition: std::collections::HashMap<i32, Vec<Record>> = std::collections::HashMap::new();
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

        let request = Request::new(ProduceRequest {
            required_acks: config.required_acks,
            timeout_ms: config.timeout_ms,
            topics: vec![produce_request::TopicData {
                topic_name: config.topic.clone(),
                partitions,
            }],
        });

        let response = client.produce(request).await?;
        for topic_result in response.results {
            for partition_result in topic_result.partitions {
                if partition_result.error_code != 0 {
                    log::warn!(
                        "Failed to send to partition {}: error_code={}",
                        partition_result.partition,
                        partition_result.error_code
                    );
                } else {
                    log::debug!(
                        "Batch sent: partition={}, offset={}",
                        partition_result.partition,
                        partition_result.offset
                    );
                }
            }
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
}
