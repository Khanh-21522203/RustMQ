use anyhow::{Context, Result};
use std::collections::HashMap;
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

/// Kafka Consumer with multi-partition support
pub struct Consumer {
    config: ConsumerConfig,
    client: Arc<KafkaBrokerClient>,
    /// Per-partition offsets
    partition_offsets: HashMap<i32, i64>,
    offsets_initialized: bool,
    /// Active partitions (from config or group assignment)
    active_partitions: Vec<i32>,
    shutdown_tx: Option<mpsc::Sender<()>>,
    is_running: bool,
}

impl Consumer {
    pub async fn new(broker_address: &str, config: ConsumerConfig) -> Result<Self> {
        let client = KafkaBrokerClient::new(broker_address)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to broker: {}", e))?;
        let active_partitions = config.partitions.clone();
        Ok(Self {
            config,
            client: Arc::new(client),
            partition_offsets: HashMap::new(),
            offsets_initialized: false,
            active_partitions,
            shutdown_tx: None,
            is_running: false,
        })
    }

    pub async fn start<H: MessageHandler + 'static>(&mut self, handler: H) -> Result<()> {
        if self.is_running {
            anyhow::bail!("Consumer is already running");
        }
        self.is_running = true;

        // Join group and get partition assignment if consumer group is configured
        if let Some(group_id) = &self.config.group_id.clone() {
            let assigned = Self::join_and_sync(&self.client, &self.config, group_id).await?;
            if !assigned.is_empty() {
                self.active_partitions = assigned;
                log::info!("Group assignment: partitions {:?}", self.active_partitions);
            }
        }

        // Resolve starting offsets for all active partitions
        let offsets = Self::resolve_starting_offsets(
            &self.client,
            &self.config,
            &self.active_partitions,
        ).await?;
        self.partition_offsets = offsets.clone();
        self.offsets_initialized = true;
        log::info!("Consumer starting from offsets: {:?}", offsets);

        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
        self.shutdown_tx = Some(shutdown_tx);

        let client = self.client.clone();
        let config = self.config.clone();
        let active_partitions = self.active_partitions.clone();
        let mut partition_offsets = offsets;

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
                        match Self::fetch_all_partitions(&client, &config, &active_partitions, &partition_offsets).await {
                            Ok(messages) => {
                                for message in messages {
                                    // Advance the per-partition offset
                                    partition_offsets.insert(message.partition, message.offset + 1);
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
                            Some(i) => Some(i.tick().await),
                            None => None,
                        }
                    } => {
                        if let Some(group_id) = &config.group_id {
                            if let Err(e) = Self::commit_all_offsets(&client, &config, group_id, &partition_offsets).await {
                                log::error!("Failed to auto-commit offsets: {}", e);
                            }
                        }
                    }

                    _ = shutdown_rx.recv() => {
                        log::info!("Consumer shutdown signal received");
                        if config.auto_commit {
                            if let Some(group_id) = &config.group_id {
                                if let Err(e) = Self::commit_all_offsets(&client, &config, group_id, &partition_offsets).await {
                                    log::error!("Failed to commit offsets on shutdown: {}", e);
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
        if !self.offsets_initialized {
            self.partition_offsets = Self::resolve_starting_offsets(
                &self.client,
                &self.config,
                &self.active_partitions,
            ).await?;
            self.offsets_initialized = true;
        }

        let messages = Self::fetch_all_partitions(
            &self.client,
            &self.config,
            &self.active_partitions,
            &self.partition_offsets,
        ).await?;

        for msg in &messages {
            self.partition_offsets.insert(msg.partition, msg.offset + 1);
        }
        Ok(messages)
    }

    pub async fn commit(&self) -> Result<()> {
        if let Some(group_id) = &self.config.group_id {
            Self::commit_all_offsets(&self.client, &self.config, group_id, &self.partition_offsets).await?;
            log::info!("Committed offsets: {:?}", self.partition_offsets);
        }
        Ok(())
    }

    pub fn current_offset(&self) -> i64 {
        // Return the lowest current offset for backward compatibility
        self.partition_offsets.values().copied().min().unwrap_or(0)
    }

    pub fn current_offsets(&self) -> &HashMap<i32, i64> {
        &self.partition_offsets
    }

    pub fn seek(&mut self, partition: i32, offset: i64) {
        self.partition_offsets.insert(partition, offset);
        self.offsets_initialized = true;
    }

    /// Join a consumer group and sync to get partition assignments.
    /// Returns the list of assigned partitions (empty if group has no partitions assigned).
    async fn join_and_sync(
        client: &KafkaBrokerClient,
        config: &ConsumerConfig,
        group_id: &str,
    ) -> Result<Vec<i32>> {
        // Generate a unique member ID
        let member_id = format!("consumer-{}", uuid_simple());

        // JoinGroup: send topic name as protocol_metadata
        let join_req = Request::new(JoinGroupRequest {
            group_id: group_id.to_string(),
            session_timeout: 30000,
            member_id: member_id.clone(),
            protocol_type: "consumer".to_string(),
            group_protocols: vec![join_group_request::GroupProtocol {
                protocol_name: "range".to_string(),
                protocol_metadata: config.topic.as_bytes().to_vec(),
            }],
        });

        let join_resp = client.join_group(join_req).await
            .map_err(|e| anyhow::anyhow!("JoinGroup failed: {}", e))?;

        if join_resp.error_code != 0 {
            anyhow::bail!("JoinGroup error_code={}", join_resp.error_code);
        }

        let actual_member_id = join_resp.member_id.clone();
        let generation_id = join_resp.generation_id;

        // SyncGroup
        let sync_req = Request::new(SyncGroupRequest {
            group_id: group_id.to_string(),
            generation_id,
            member_id: actual_member_id,
            group_assignment: vec![],
        });

        let sync_resp = client.sync_group(sync_req).await
            .map_err(|e| anyhow::anyhow!("SyncGroup failed: {}", e))?;

        if sync_resp.error_code != 0 {
            anyhow::bail!("SyncGroup error_code={}", sync_resp.error_code);
        }

        // Decode assigned partitions from bincode
        if sync_resp.member_assignment.is_empty() {
            return Ok(vec![]);
        }

        let partitions: Vec<i32> = bincode::deserialize(&sync_resp.member_assignment)
            .unwrap_or_default();
        Ok(partitions)
    }

    async fn resolve_starting_offsets(
        client: &KafkaBrokerClient,
        config: &ConsumerConfig,
        partitions: &[i32],
    ) -> Result<HashMap<i32, i64>> {
        let mut result = HashMap::new();

        for &partition in partitions {
            let offset = Self::resolve_partition_offset(client, config, partition).await?;
            result.insert(partition, offset);
        }

        Ok(result)
    }

    async fn resolve_partition_offset(
        client: &KafkaBrokerClient,
        config: &ConsumerConfig,
        partition: i32,
    ) -> Result<i64> {
        // Specific offset provided
        if config.offset >= 0 {
            return Ok(config.offset);
        }

        // Try committed offset
        if let Some(group_id) = &config.group_id {
            let request = Request::new(OffsetFetchRequest {
                consumer_group_id: group_id.clone(),
                topics: vec![offset_fetch_request::TopicData {
                    topic_name: config.topic.clone(),
                    partitions: vec![partition],
                }],
            });
            match client.fetch_offset(request).await {
                Ok(response) => {
                    for topic_result in response.topics {
                        for partition_result in topic_result.partitions {
                            if partition_result.error_code == 0 && partition_result.offset >= 0 {
                                return Ok(partition_result.offset + 1);
                            }
                        }
                    }
                }
                Err(e) => {
                    log::warn!("Failed to fetch committed offset for partition {}: {}", partition, e);
                }
            }
        }

        // Fall back to ListOffsets sentinel
        let request = Request::new(ListOffsetsRequest {
            replica_id: -1,
            topics: vec![list_offsets_request::TopicData {
                topic_name: config.topic.clone(),
                partitions: vec![list_offsets_request::PartitionData {
                    partition,
                    time: config.offset, // -1=latest, -2=earliest
                    max_number_of_offsets: 1,
                }],
            }],
        });
        let response = client.list_offsets(request).await?;
        for topic_result in response.topics {
            for partition_offsets in topic_result.partitions {
                if partition_offsets.error_code == 0 && !partition_offsets.offsets.is_empty() {
                    return Ok(partition_offsets.offsets[0]);
                }
            }
        }
        Ok(0)
    }

    async fn fetch_all_partitions(
        client: &KafkaBrokerClient,
        config: &ConsumerConfig,
        partitions: &[i32],
        offsets: &HashMap<i32, i64>,
    ) -> Result<Vec<ConsumedMessage>> {
        let partition_data: Vec<fetch_request::PartitionData> = partitions
            .iter()
            .map(|&p| fetch_request::PartitionData {
                partition: p,
                fetch_offset: *offsets.get(&p).unwrap_or(&0),
                max_bytes: config.max_bytes,
            })
            .collect();

        let request = Request::new(FetchRequest {
            replica_id: -1,
            max_wait_time: config.max_wait_ms,
            min_bytes: config.min_bytes,
            topics: vec![fetch_request::TopicData {
                topic_name: config.topic.clone(),
                partitions: partition_data,
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
                        partition: partition_result.partition,
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

    async fn commit_all_offsets(
        client: &KafkaBrokerClient,
        config: &ConsumerConfig,
        group_id: &str,
        offsets: &HashMap<i32, i64>,
    ) -> Result<()> {
        let partitions: Vec<offset_commit_request::PartitionData> = offsets
            .iter()
            .map(|(&partition, &offset)| offset_commit_request::PartitionData {
                partition,
                offset,
                metadata: String::new(),
            })
            .collect();

        let request = Request::new(OffsetCommitRequest {
            consumer_group_id: group_id.to_string(),
            topics: vec![offset_commit_request::TopicData {
                topic_name: config.topic.clone(),
                partitions,
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

/// Simple pseudo-unique ID for consumer member IDs (no uuid dep needed)
fn uuid_simple() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    format!("{}-{}", t.as_secs(), t.subsec_nanos())
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
