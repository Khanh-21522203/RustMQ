use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio::sync::Mutex;
use tonic::Request;
use prost_raft::Message as RaftProstMessage;
use raft::eraftpb::Message;

use crate::broker::raft_transport::RaftTransport;

pub mod raft_proto {
    tonic::include_proto!("raft");
}

use raft_proto::raft_client::RaftClient;

// ── Peer info ─────────────────────────────────────────────────────────────────

/// Addresses for a single cluster peer.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PeerInfo {
    /// gRPC endpoint for Raft inter-broker messages (must include scheme, e.g. "http://…")
    pub rpc_addr: String,
    /// gRPC endpoint for client-facing API (e.g. "127.0.0.1:9092")
    pub api_addr: String,
    /// Raw TCP address for SBE+TCP transport (e.g. "host:29092"). None = unused.
    pub sbe_tcp_addr: Option<String>,
}

// ── Outbound: send Raft messages to peers ─────────────────────────────────────

/// Sends outgoing Raft protocol messages to peer brokers over gRPC.
#[derive(Clone)]
pub struct RaftNetworkSender {
    node_id: u64,
    /// node_id → peer addresses (shared with RaftStorage for dynamic updates)
    peers: Arc<RwLock<HashMap<u64, PeerInfo>>>,
    /// Lazy connection pool keyed by node_id
    pool: Arc<Mutex<HashMap<u64, RaftClient<tonic::transport::Channel>>>>,
}

impl RaftNetworkSender {
    pub fn new(node_id: u64, peers: Arc<RwLock<HashMap<u64, PeerInfo>>>) -> Self {
        Self {
            node_id,
            peers,
            pool: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn send_messages(&self, msgs: Vec<Message>) {
        use raft_proto::{RaftMessage};
        for msg in msgs {
            if msg.to == self.node_id {
                continue; // never send to self
            }
            let addr = {
                let peers = self.peers.read().unwrap();
                match peers.get(&msg.to) {
                    Some(p) => p.rpc_addr.clone(),
                    None => {
                        log::warn!("No address for peer node {}", msg.to);
                        continue;
                    }
                }
            };

            let data = RaftProstMessage::encode_to_vec(&msg);
            let request = RaftMessage {
                rpc_type: "raft".into(),
                data,
            };

            let client = self.get_or_connect(msg.to, addr).await;
            match client {
                Ok(mut c) => {
                    if let Err(e) = c.send_raft(Request::new(request)).await {
                        log::warn!("Failed to send raft message to node {}: {}", msg.to, e);
                    }
                }
                Err(e) => {
                    log::warn!("Could not connect to peer node {}: {}", msg.to, e);
                }
            }
        }
    }

    async fn get_or_connect(
        &self,
        node_id: u64,
        addr: String,
    ) -> anyhow::Result<RaftClient<tonic::transport::Channel>> {
        let mut pool = self.pool.lock().await;
        if let Some(client) = pool.get(&node_id) {
            return Ok(client.clone());
        }
        let endpoint = tonic::transport::Endpoint::from_shared(addr)?;
        let channel = endpoint.connect_lazy();
        let client = RaftClient::new(channel);
        pool.insert(node_id, client.clone());
        Ok(client)
    }
}

// ── GrpcTransport: implements RaftTransport over the existing gRPC sender ─────

/// Wraps `RaftNetworkSender` to satisfy the `RaftTransport` trait.
pub struct GrpcTransport(pub RaftNetworkSender);

#[async_trait::async_trait]
impl RaftTransport for GrpcTransport {
    async fn send_messages(&self, msgs: Vec<Message>) {
        self.0.send_messages(msgs).await;
    }
}
