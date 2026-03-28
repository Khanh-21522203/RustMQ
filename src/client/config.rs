use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Main configuration structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub broker: BrokerConfig,

    #[serde(default)]
    pub producer: Option<ProducerConfig>,

    #[serde(default)]
    pub consumer: Option<ConsumerConfig>,
}

/// Broker configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrokerConfig {
    /// Broker address (e.g., "http://localhost:50051")
    #[serde(default = "default_broker_address")]
    pub address: String,

    /// Connection timeout in seconds
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,

    /// Maximum retries for failed operations
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
}

impl Default for BrokerConfig {
    fn default() -> Self {
        Self {
            address: default_broker_address(),
            timeout_secs: default_timeout(),
            max_retries: default_max_retries(),
        }
    }
}

/// Producer configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProducerConfig {
    /// Topic to produce to
    pub topic: String,

    /// Default partition (used when partitioning = "fixed")
    #[serde(default)]
    pub partition: i32,

    /// Partitioning strategy: "fixed", "round_robin", or "key_hash"
    #[serde(default = "default_partitioning")]
    pub partitioning: String,

    /// Number of partitions for round_robin / key_hash strategies
    #[serde(default = "default_num_partitions")]
    pub num_partitions: i32,

    /// Required acknowledgments (-1=all, 0=none, 1=leader)
    #[serde(default = "default_acks")]
    pub required_acks: i32,

    /// Timeout for produce requests in milliseconds
    #[serde(default = "default_produce_timeout")]
    pub timeout_ms: i32,

    /// Batch size for batching messages
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,

    /// Flush interval in milliseconds
    #[serde(default = "default_flush_interval")]
    pub flush_interval_ms: u64,
}

/// Consumer configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsumerConfig {
    /// Topic to consume from
    pub topic: String,

    /// Partitions to consume from (default: [0])
    #[serde(default = "default_partitions")]
    pub partitions: Vec<i32>,

    /// Consumer group ID
    pub group_id: Option<String>,

    /// Starting offset (-2=earliest, -1=latest, >=0=specific offset)
    #[serde(default)]
    pub offset: i64,

    /// Maximum bytes to fetch per request
    #[serde(default = "default_max_bytes")]
    pub max_bytes: i32,

    /// Maximum wait time in milliseconds
    #[serde(default = "default_max_wait")]
    pub max_wait_ms: i32,

    /// Minimum bytes to wait for
    #[serde(default = "default_min_bytes")]
    pub min_bytes: i32,

    /// Auto-commit offsets
    #[serde(default)]
    pub auto_commit: bool,

    /// Auto-commit interval in milliseconds
    #[serde(default = "default_auto_commit_interval")]
    pub auto_commit_interval_ms: u64,

    /// Poll interval in milliseconds
    #[serde(default = "default_poll_interval")]
    pub poll_interval_ms: u64,

    /// Maximum number of records processed per poll cycle.
    /// Acts as backpressure for both fetch volume and handler throughput.
    #[serde(default = "default_max_messages_per_poll")]
    pub max_messages_per_poll: usize,
}

// Default value functions
fn default_broker_address() -> String {
    "http://localhost:50051".to_string()
}

fn default_timeout() -> u64 {
    30
}

fn default_max_retries() -> u32 {
    3
}

fn default_acks() -> i32 {
    1
}

fn default_produce_timeout() -> i32 {
    5000
}

fn default_batch_size() -> usize {
    100
}

fn default_flush_interval() -> u64 {
    100
}

fn default_max_bytes() -> i32 {
    1_048_576 // 1MB
}

fn default_max_wait() -> i32 {
    1000
}

fn default_min_bytes() -> i32 {
    1
}

fn default_auto_commit_interval() -> u64 {
    5000
}

fn default_poll_interval() -> u64 {
    1000
}

fn default_max_messages_per_poll() -> usize {
    1000
}

fn default_partitions() -> Vec<i32> {
    vec![0]
}

fn default_partitioning() -> String {
    "fixed".to_string()
}

fn default_num_partitions() -> i32 {
    1
}

impl AppConfig {
    /// Load configuration from YAML file
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content =
            std::fs::read_to_string(path.as_ref()).context("Failed to read config file")?;

        let config: AppConfig =
            serde_yaml::from_str(&content).context("Failed to parse YAML config")?;

        Ok(config)
    }

    /// Load configuration from YAML string
    pub fn from_yaml_str(yaml: &str) -> Result<Self> {
        serde_yaml::from_str(yaml).context("Failed to parse YAML")
    }

    /// Create default configuration
    pub fn default_producer(topic: impl Into<String>) -> Self {
        Self {
            broker: BrokerConfig::default(),
            producer: Some(ProducerConfig {
                topic: topic.into(),
                partition: 0,
                partitioning: default_partitioning(),
                num_partitions: default_num_partitions(),
                required_acks: default_acks(),
                timeout_ms: default_produce_timeout(),
                batch_size: default_batch_size(),
                flush_interval_ms: default_flush_interval(),
            }),
            consumer: None,
        }
    }

    /// Create default consumer configuration
    pub fn default_consumer(topic: impl Into<String>, group_id: Option<String>) -> Self {
        Self {
            broker: BrokerConfig::default(),
            producer: None,
            consumer: Some(ConsumerConfig {
                topic: topic.into(),
                partitions: default_partitions(),
                group_id,
                offset: -2, // Start from earliest
                max_bytes: default_max_bytes(),
                max_wait_ms: default_max_wait(),
                min_bytes: default_min_bytes(),
                auto_commit: false,
                auto_commit_interval_ms: default_auto_commit_interval(),
                poll_interval_ms: default_poll_interval(),
                max_messages_per_poll: default_max_messages_per_poll(),
            }),
        }
    }

    /// Validate configuration
    pub fn validate(&self) -> Result<()> {
        if let Some(producer) = &self.producer {
            if producer.topic.is_empty() {
                anyhow::bail!("Producer topic cannot be empty");
            }
            validate_topic_name(&producer.topic)
                .map_err(|e| anyhow::anyhow!("Producer config: {}", e))?;
        }

        if let Some(consumer) = &self.consumer {
            if consumer.topic.is_empty() {
                anyhow::bail!("Consumer topic cannot be empty");
            }
            validate_topic_name(&consumer.topic)
                .map_err(|e| anyhow::anyhow!("Consumer config: {}", e))?;
        }

        Ok(())
    }
}

/// Validate a topic name. Only alphanumeric characters, '.', '-', and '_' are allowed.
/// This mirrors the broker-side validation and prevents injection via sled key format.
fn validate_topic_name(topic: &str) -> Result<(), String> {
    if topic.is_empty() {
        return Err("topic name cannot be empty".to_string());
    }
    if !topic
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    {
        return Err(format!(
            "invalid topic name '{}': only alphanumeric characters, '.', '-', and '_' are allowed",
            topic
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_configs() {
        let producer_config = AppConfig::default_producer("test-topic");
        assert_eq!(producer_config.producer.unwrap().topic, "test-topic");

        let consumer_config = AppConfig::default_consumer("test-topic", Some("group1".to_string()));
        assert_eq!(consumer_config.consumer.unwrap().topic, "test-topic");
    }

    #[test]
    fn test_validate_rejects_invalid_topic_names() {
        let mut cfg = AppConfig::default_producer("bad:topic");
        assert!(cfg.validate().is_err());

        cfg = AppConfig::default_producer("bad topic");
        assert!(cfg.validate().is_err());

        cfg = AppConfig::default_producer("good-topic.v2_ok");
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn test_yaml_parsing() {
        let yaml = r#"
broker:
  address: "http://localhost:9092"
  timeout_secs: 60

producer:
  topic: "my-topic"
  required_acks: 1
"#;
        let config = AppConfig::from_yaml_str(yaml).unwrap();
        assert_eq!(config.broker.address, "http://localhost:9092");
        assert_eq!(config.producer.unwrap().topic, "my-topic");
    }
}
