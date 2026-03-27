use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tonic::Request;

use crate::api::broker::TopicMetadataRequest;
use crate::client::kafka_broker_client::{KafkaBrokerClient, KafkaBrokerClientTrait};

/// Normalise a broker address to "host:port" (strip any leading "http://").
fn strip_scheme(addr: &str) -> &str {
    addr.strip_prefix("http://").unwrap_or(addr)
}

/// Client-side metadata cache.
///
/// Stores a `(topic, partition) → broker_addr` map and a connection pool so
/// that the producer can route each produce request directly to the partition
/// leader without going through a broker that would then return NOT_LEADER.
///
/// All addresses stored internally are in canonical `"host:port"` form.
/// `KafkaBrokerClient::new` accepts both `"host:port"` and `"http://host:port"`.
pub struct MetadataCache {
    /// Bootstrap broker used for metadata requests.  Stored in canonical form.
    bootstrap_addr: String,
    /// (topic, partition_id) → canonical broker address.
    leaders: HashMap<(String, i32), String>,
    /// Canonical broker address → live gRPC client.
    connections: HashMap<String, Arc<KafkaBrokerClient>>,
}

impl MetadataCache {
    pub fn new(bootstrap_addr: &str) -> Self {
        Self {
            bootstrap_addr: strip_scheme(bootstrap_addr).to_string(),
            leaders: HashMap::new(),
            connections: HashMap::new(),
        }
    }

    /// Return the cached leader address for a partition, or `None` if the cache
    /// is cold / has been invalidated for this topic.
    pub fn get_leader(&self, topic: &str, partition: i32) -> Option<&str> {
        self.leaders
            .get(&(topic.to_string(), partition))
            .map(|s| s.as_str())
    }

    /// Drop all cached leader entries for `topic`, forcing a re-fetch on the
    /// next [`resolve_leaders`] call.
    pub fn invalidate_topic(&mut self, topic: &str) {
        self.leaders.retain(|(t, _), _| t != topic);
    }

    /// Fetch fresh `TopicMetadata` for `topic` from the bootstrap broker and
    /// update the leader cache.  Partitions whose leader is unknown (e.g.
    /// during an election) are skipped — they stay absent from the cache so the
    /// next call will fetch again.
    pub async fn refresh(&mut self, topic: &str) -> Result<()> {
        let client = self.get_or_connect(&self.bootstrap_addr.clone()).await?;
        let resp = client
            .get_topic_metadata(Request::new(TopicMetadataRequest {
                topics: vec![topic.to_string()],
            }))
            .await
            .map_err(|e| anyhow::anyhow!("Metadata fetch failed: {}", e))?;

        // Build node_id → canonical address from the broker list.
        let broker_addrs: HashMap<i32, String> = resp
            .brokers
            .iter()
            .map(|b| (b.node_id, format!("{}:{}", b.host, b.port)))
            .collect();

        for topic_meta in &resp.topics {
            if topic_meta.topic_error_code != 0 {
                continue;
            }
            for pm in &topic_meta.partitions {
                if pm.partition_error_code != 0 {
                    // Leader not yet elected — leave absent so we retry.
                    log::debug!(
                        "Partition {}-{} has error_code {}; skipping cache entry",
                        topic_meta.topic_name,
                        pm.partition_id,
                        pm.partition_error_code
                    );
                    continue;
                }
                if let Some(addr) = broker_addrs.get(&pm.leader) {
                    self.leaders
                        .insert((topic_meta.topic_name.clone(), pm.partition_id), addr.clone());
                    log::debug!(
                        "Cached leader for {}-{}: {}",
                        topic_meta.topic_name,
                        pm.partition_id,
                        addr
                    );
                } else {
                    log::warn!(
                        "Leader node_id {} for {}-{} not found in broker list",
                        pm.leader,
                        topic_meta.topic_name,
                        pm.partition_id
                    );
                }
            }
        }
        Ok(())
    }

    /// Return a live client for `addr`, creating and caching a new gRPC
    /// connection if one does not already exist.
    pub async fn get_or_connect(&mut self, addr: &str) -> Result<Arc<KafkaBrokerClient>> {
        let key = strip_scheme(addr).to_string();
        if let Some(c) = self.connections.get(&key) {
            return Ok(c.clone());
        }
        let client = Arc::new(
            KafkaBrokerClient::new(&key)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to connect to {}: {}", key, e))?,
        );
        self.connections.insert(key, client.clone());
        Ok(client)
    }

    /// For each partition in `partitions`, look up (or fetch) the leader and
    /// return a map of `broker_addr → Vec<partition_id>`.
    ///
    /// A single `refresh` is attempted when any partition is absent.
    pub async fn resolve_leaders(
        &mut self,
        topic: &str,
        partitions: &[i32],
    ) -> Result<HashMap<String, Vec<i32>>> {
        // One refresh attempt if the cache is cold for any partition.
        let needs_refresh = partitions
            .iter()
            .any(|&p| self.get_leader(topic, p).is_none());
        if needs_refresh {
            self.refresh(topic).await?;
        }

        let mut by_broker: HashMap<String, Vec<i32>> = HashMap::new();
        for &partition in partitions {
            let addr = self
                .get_leader(topic, partition)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "No leader found for {}-{} even after metadata refresh",
                        topic,
                        partition
                    )
                })?
                .to_string();
            by_broker.entry(addr).or_default().push(partition);
        }
        Ok(by_broker)
    }
}
