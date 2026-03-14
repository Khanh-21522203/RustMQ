use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{interval, sleep, Duration};
use tonic::Request;

use crate::api::broker::*;
use crate::client::config::ConsumerConfig;
use crate::client::kafka_broker_client::{KafkaBrokerClient, KafkaBrokerClientTrait};

/// Consumed message
#[derive(Debug, Clone)]
pub struct ConsumedMessage {
    pub topic: String,
    pub partition: i32,
    pub offset: i64,
    pub key: Option<Vec<u8>>,
    pub value: Vec<u8>,
    pub timestamp: Option<i64>,
}

impl ConsumedMessage {
    pub fn value_as_string(&self) -> Result<String> {
        String::from_utf8(self.value.clone())
            .context("Failed to convert message value to string")
    }
}

/// Message handler trait for processing consumed messages.
/// Takes &self so the handler can be shared across tasks.
#[async_trait::async_trait]
pub trait MessageHandler: Send + Sync {
    async fn handle(&self, message: ConsumedMessage) -> Result<()>;
}

/// Simple function-based message handler
pub struct FnHandler<F>
where
    F: Fn(ConsumedMessage) -> Result<()> + Send + Sync,
{
    handler: F,
}

impl<F> FnHandler<F>
where
    F: Fn(ConsumedMessage) -> Result<()> + Send + Sync,
{
    pub fn new(handler: F) -> Self {
        Self { handler }
    }
}

#[async_trait::async_trait]
impl<F> MessageHandler for FnHandler<F>
where
    F: Fn(ConsumedMessage) -> Result<()> + Send + Sync,
{
    async fn handle(&self, message: ConsumedMessage) -> Result<()> {
        (self.handler)(message)
    }
}

/// Kafka Consumer
pub struct Consumer {
    config: ConsumerConfig,
    client: Arc<KafkaBrokerClient>,
    current_offset: i64,
    offset_initialized: bool,
    shutdown_tx: Option<mpsc::Sender<()>>,
    is_running: bool,
}

impl Consumer {
    pub async fn new(broker_address: &str, config: ConsumerConfig) -> Result<Self> {
        let client = KafkaBrokerClient::new(broker_address)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to broker: {}", e))?;
        Ok(Self {
            config,
            client: Arc::new(client),
            current_offset: 0,
            offset_initialized: false,
            shutdown_tx: None,
            is_running: false,
        })
    }

    pub async fn start<H: MessageHandler + 'static>(&mut self, handler: H) -> Result<()> {
        if self.is_running {
            anyhow::bail!("Consumer is already running");
        }
        self.is_running = true;

        // Resolve starting offset before spawning background task
        let starting_offset = self.resolve_starting_offset().await?;
        self.current_offset = starting_offset;
        self.offset_initialized = true;
        log::info!("Consumer starting from offset: {}", starting_offset);

        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
        self.shutdown_tx = Some(shutdown_tx);

        let client = self.client.clone();
        let config = self.config.clone();
        let mut current_offset = starting_offset;

        tokio::spawn(async move {
            let mut poll_interval = interval(Duration::from_millis(config.poll_interval_ms));
            let mut auto_commit_interval = if config.auto_commit {
                Some(interval(Duration::from_millis(config.auto_commit_interval_ms)))
            } else {
                None
            };

            loop {
                tokio::select! {
                    _ = poll_interval.tick() => {
                        match Self::fetch_messages(&client, &config, current_offset).await {
                            Ok(messages) => {
                                if messages.is_empty() {
                                    continue;
                                }
                                for message in messages {
                                    current_offset = message.offset + 1;
                                    if let Err(e) = handler.handle(message).await {
                                        log::error!("Error handling message: {}", e);
                                    }
                                }
                            }
                            Err(e) => {
                                log::error!("Failed to fetch messages: {}", e);
                                sleep(Duration::from_secs(1)).await;
                            }
                        }
                    }

                    Some(_) = async {
                        match &mut auto_commit_interval {
                            Some(interval) => Some(interval.tick().await),
                            None => None,
                        }
                    } => {
                        if let Some(group_id) = &config.group_id {
                            if let Err(e) = Self::commit_offset(&client, &config, group_id, current_offset).await {
                                log::error!("Failed to auto-commit offset: {}", e);
                            }
                        }
                    }

                    _ = shutdown_rx.recv() => {
                        log::info!("Consumer shutdown signal received");
                        if config.auto_commit {
                            if let Some(group_id) = &config.group_id {
                                if let Err(e) = Self::commit_offset(&client, &config, group_id, current_offset).await {
                                    log::error!("Failed to commit offset on shutdown: {}", e);
                                }
                            }
                        }
                        break;
                    }
                }
            }
            log::info!("Consumer stopped");
        });

        log::info!("Consumer started for topic: {}", self.config.topic);
        Ok(())
    }

    pub async fn poll(&mut self) -> Result<Vec<ConsumedMessage>> {
        if !self.offset_initialized {
            self.current_offset = self.resolve_starting_offset().await?;
            self.offset_initialized = true;
        }

        let messages = Self::fetch_messages(&self.client, &self.config, self.current_offset).await?;
        if !messages.is_empty() {
            self.current_offset = messages.last().unwrap().offset + 1;
        }
        Ok(messages)
    }

    pub async fn commit(&self) -> Result<()> {
        if let Some(group_id) = &self.config.group_id {
            Self::commit_offset(&self.client, &self.config, group_id, self.current_offset).await?;
            log::info!("Committed offset: {}", self.current_offset);
        }
        Ok(())
    }

    pub fn current_offset(&self) -> i64 {
        self.current_offset
    }

    pub fn seek(&mut self, offset: i64) {
        self.current_offset = offset;
        self.offset_initialized = true;
    }

    async fn resolve_starting_offset(&self) -> Result<i64> {
        // Specific offset provided
        if self.config.offset >= 0 {
            return Ok(self.config.offset);
        }

        // Try committed offset (resume after last committed + 1)
        if let Some(group_id) = &self.config.group_id {
            let request = Request::new(OffsetFetchRequest {
                consumer_group_id: group_id.clone(),
                topics: vec![offset_fetch_request::TopicData {
                    topic_name: self.config.topic.clone(),
                    partitions: vec![self.config.partition],
                }],
            });
            match self.client.fetch_offset(request).await {
                Ok(response) => {
                    for topic_result in response.topics {
                        for partition_result in topic_result.partitions {
                            if partition_result.error_code == 0 && partition_result.offset >= 0 {
                                // +1 to skip the already-processed message
                                let resume = partition_result.offset + 1;
                                log::info!("Resuming from committed offset + 1 = {}", resume);
                                return Ok(resume);
                            }
                        }
                    }
                }
                Err(e) => {
                    log::warn!("Failed to fetch committed offset: {}", e);
                }
            }
        }

        // Fall back to ListOffsets sentinel
        let request = Request::new(ListOffsetsRequest {
            replica_id: -1,
            topics: vec![list_offsets_request::TopicData {
                topic_name: self.config.topic.clone(),
                partitions: vec![list_offsets_request::PartitionData {
                    partition: self.config.partition,
                    time: self.config.offset, // -1=latest, -2=earliest
                    max_number_of_offsets: 1,
                }],
            }],
        });
        let response = self.client.list_offsets(request).await?;
        for topic_result in response.topics {
            for partition_offsets in topic_result.partitions {
                if partition_offsets.error_code == 0 && !partition_offsets.offsets.is_empty() {
                    return Ok(partition_offsets.offsets[0]);
                }
            }
        }
        Ok(0)
    }

    async fn fetch_messages(
        client: &KafkaBrokerClient,
        config: &ConsumerConfig,
        offset: i64,
    ) -> Result<Vec<ConsumedMessage>> {
        let request = Request::new(FetchRequest {
            replica_id: -1,
            max_wait_time: config.max_wait_ms,
            min_bytes: config.min_bytes,
            topics: vec![fetch_request::TopicData {
                topic_name: config.topic.clone(),
                partitions: vec![fetch_request::PartitionData {
                    partition: config.partition,
                    fetch_offset: offset,
                    max_bytes: config.max_bytes,
                }],
            }],
        });

        let response = client.fetch(request).await?;
        let mut messages = Vec::new();

        for topic_result in response.topics {
            for partition_result in topic_result.partitions {
                if partition_result.error_code != 0 {
                    continue;
                }
                for record in partition_result.records {
                    messages.push(ConsumedMessage {
                        topic: config.topic.clone(),
                        partition: config.partition,
                        offset: record.offset,
                        key: record.key,
                        value: record.value,
                        timestamp: None,
                    });
                }
            }
        }

        Ok(messages)
    }

    async fn commit_offset(
        client: &KafkaBrokerClient,
        config: &ConsumerConfig,
        group_id: &str,
        offset: i64,
    ) -> Result<()> {
        let request = Request::new(OffsetCommitRequest {
            consumer_group_id: group_id.to_string(),
            topics: vec![offset_commit_request::TopicData {
                topic_name: config.topic.clone(),
                partitions: vec![offset_commit_request::PartitionData {
                    partition: config.partition,
                    offset,
                    metadata: String::new(),
                }],
            }],
        });
        client.commit_offset(request).await?;
        Ok(())
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        if !self.is_running {
            return Ok(());
        }
        log::info!("Shutting down consumer...");
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(()).await;
        }
        self.is_running = false;
        log::info!("Consumer shutdown complete");
        Ok(())
    }
}

impl Drop for Consumer {
    fn drop(&mut self) {
        log::debug!("Consumer dropped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_consumed_message() {
        let msg = ConsumedMessage {
            topic: "test".to_string(),
            partition: 0,
            offset: 100,
            key: None,
            value: b"hello".to_vec(),
            timestamp: None,
        };
        assert_eq!(msg.value_as_string().unwrap(), "hello");
        assert_eq!(msg.offset, 100);
    }
}
