use anyhow::Result;
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
    /// Create a new message with value
    pub fn new(value: impl Into<Vec<u8>>) -> Self {
        Self {
            key: None,
            value: value.into(),
            partition: None,
        }
    }
    
    /// Create a message with key and value
    pub fn with_key(key: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>) -> Self {
        Self {
            key: Some(key.into()),
            value: value.into(),
            partition: None,
        }
    }
    
    /// Set the target partition
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
    shutdown_tx: Option<mpsc::Sender<()>>,
}

impl Producer {
    /// Create a new producer
    pub async fn new(broker_address: &str, config: ProducerConfig) -> Result<Self> {
        let client = KafkaBrokerClient::new(broker_address)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to broker: {}", e))?;
        
        Ok(Self {
            config,
            client: Arc::new(client),
            batch: Arc::new(Mutex::new(Vec::new())),
            shutdown_tx: None,
        })
    }
    
    /// Start the producer with automatic flushing
    pub async fn start(&mut self) -> Result<()> {
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel(1);
        self.shutdown_tx = Some(shutdown_tx);
        
        let batch = self.batch.clone();
        let client = self.client.clone();
        let config = self.config.clone();
        
        tokio::spawn(async move {
            let mut flush_interval = interval(Duration::from_millis(config.flush_interval_ms));
            
            loop {
                tokio::select! {
                    _ = flush_interval.tick() => {
                        let messages = {
                            let mut batch_lock = batch.lock().await;
                            if batch_lock.is_empty() {
                                continue;
                            }
                            std::mem::take(&mut *batch_lock)
                        };
                        
                        if let Err(e) = Self::send_batch(&client, &config, messages).await {
                            log::error!("Failed to flush batch: {}", e);
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        log::info!("Producer shutdown signal received");
                        
                        // Final flush
                        let messages = {
                            let mut batch_lock = batch.lock().await;
                            std::mem::take(&mut *batch_lock)
                        };
                        
                        if !messages.is_empty() {
                            if let Err(e) = Self::send_batch(&client, &config, messages).await {
                                log::error!("Failed to flush final batch: {}", e);
                            }
                        }
                        
                        break;
                    }
                }
            }
            
            log::info!("Producer background task stopped");
        });
        
        log::info!("Producer started for topic: {}", self.config.topic);
        Ok(())
    }
    
    /// Send a single message (adds to batch)
    pub async fn send(&self, message: ProducerMessage) -> Result<()> {
        let mut batch = self.batch.lock().await;
        batch.push(message);
        
        // Check if batch is full
        if batch.len() >= self.config.batch_size {
            let messages = std::mem::take(&mut *batch);
            drop(batch); // Release lock before sending
            
            Self::send_batch(&self.client, &self.config, messages).await?;
        }
        
        Ok(())
    }
    
    /// Send a message and wait for result (synchronous mode)
    pub async fn send_sync(&self, message: ProducerMessage) -> Result<ProducerResult> {
        let partition = message.partition.unwrap_or(self.config.partition);
        
        let request = Request::new(ProduceRequest {
            required_acks: self.config.required_acks,
            timeout_ms: self.config.timeout_ms,
            topics: vec![produce_request::TopicData {
                topic_name: self.config.topic.clone(),
                partitions: vec![produce_request::PartitionData {
                    partition,
                    message_set: message.value,
                }],
            }],
        });
        
        let response = self.client.produce(request).await?;
        
        for topic_result in response.results {
            for partition_result in topic_result.partitions {
                if partition_result.error_code == 0 {
                    log::debug!(
                        "Message sent successfully: partition={}, offset={}",
                        partition_result.partition,
                        partition_result.offset
                    );
                    
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
    
    /// Flush any pending messages
    pub async fn flush(&self) -> Result<()> {
        let messages = {
            let mut batch = self.batch.lock().await;
            std::mem::take(&mut *batch)
        };
        
        if !messages.is_empty() {
            Self::send_batch(&self.client, &self.config, messages).await?;
        }
        
        Ok(())
    }
    
    /// Send a batch of messages
    async fn send_batch(
        client: &KafkaBrokerClient,
        config: &ProducerConfig,
        messages: Vec<ProducerMessage>,
    ) -> Result<()> {
        if messages.is_empty() {
            return Ok(());
        }
        
        let partition_data: Vec<_> = messages
            .into_iter()
            .map(|msg| produce_request::PartitionData {
                partition: msg.partition.unwrap_or(config.partition),
                message_set: msg.value,
            })
            .collect();
        
        let request = Request::new(ProduceRequest {
            required_acks: config.required_acks,
            timeout_ms: config.timeout_ms,
            topics: vec![produce_request::TopicData {
                topic_name: config.topic.clone(),
                partitions: partition_data,
            }],
        });
        
        let response = client.produce(request).await?;
        
        for topic_result in response.results {
            for partition_result in topic_result.partitions {
                if partition_result.error_code != 0 {
                    log::warn!(
                        "Failed to send message to partition {}: error_code={}",
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
    
    /// Shutdown the producer gracefully
    pub async fn shutdown(&mut self) -> Result<()> {
        log::info!("Shutting down producer...");
        
        // Flush remaining messages
        self.flush().await?;
        
        // Signal background task to stop
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(()).await;
        }
        
        log::info!("Producer shutdown complete");
        Ok(())
    }
}

impl Drop for Producer {
    fn drop(&mut self) {
        log::debug!("Producer dropped");
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
