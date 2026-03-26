use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tonic::{Request, Response, Status};
// Use prost 0.11 (the version raft uses) to encode/decode eraftpb::Message
use prost_raft::Message as RaftProstMessage;

pub mod raft_proto {
    tonic::include_proto!("raft");
}

use raft::eraftpb::Message;
use raft_proto::raft_client::RaftClient;
use raft_proto::raft_server::Raft;
use raft_proto::{RaftMessage, RaftReply};

// ── Peer info ─────────────────────────────────────────────────────────────────

/// Addresses for a single cluster peer.
#[derive(Debug, Clone)]
pub struct PeerInfo {
    /// gRPC endpoint for Raft inter-broker messages (must include scheme, e.g. "http://…")
    pub rpc_addr: String,
    /// gRPC endpoint for client-facing API (e.g. "127.0.0.1:9092")
    pub api_addr: String,
}

// ── Outbound: send Raft messages to peers ─────────────────────────────────────

/// Sends outgoing Raft protocol messages to peer brokers over gRPC.
#[derive(Clone)]
pub struct RaftNetworkSender {
    node_id: u64,
    /// node_id → peer addresses
    peers: HashMap<u64, PeerInfo>,
    /// Lazy connection pool keyed by node_id
    pool: Arc<Mutex<HashMap<u64, RaftClient<tonic::transport::Channel>>>>,
}

impl RaftNetworkSender {
    pub fn new(node_id: u64, peers: HashMap<u64, PeerInfo>) -> Self {
        Self {
            node_id,
            peers,
            pool: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Return the API address of a peer node (for leader-redirect hints).
    pub fn peer_api_addr(&self, node_id: u64) -> Option<&str> {
        self.peers.get(&node_id).map(|p| p.api_addr.as_str())
    }

    pub async fn send_messages(&self, msgs: Vec<Message>) {
        for msg in msgs {
            if msg.to == self.node_id {
                continue; // never send to self
            }
            let Some(peer) = self.peers.get(&msg.to) else {
                log::warn!("No address for peer node {}", msg.to);
                continue;
            };
            let addr = peer.rpc_addr.clone();

            let data = RaftProstMessage::encode_to_vec(&msg);
            let request = RaftMessage {
                rpc_type: "raft".into(),
                data,
            };

            let client = self.get_or_connect(msg.to, addr).await;
            match client {
                Ok(mut c) => {
                    if let Err(e) = c.send_raft(Request::new(request)).await {
                        log::debug!("Failed to send raft message to node {}: {}", msg.to, e);
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
        addr: String, // rpc_addr
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

// ── Inbound: receive Raft messages from peers ─────────────────────────────────

/// tonic service implementation that receives Raft messages from peers
/// and forwards them to the local RawNode via a channel.
pub struct RaftServiceImpl {
    step_tx: mpsc::UnboundedSender<Message>,
}

#[tonic::async_trait]
impl Raft for RaftServiceImpl {
    async fn send_raft(
        &self,
        request: Request<RaftMessage>,
    ) -> Result<Response<RaftReply>, Status> {
        let data = request.into_inner().data;
        match <Message as RaftProstMessage>::decode(data.as_slice()) {
            Ok(msg) => {
                let _ = self.step_tx.send(msg);
                Ok(Response::new(RaftReply::default()))
            }
            Err(e) => Err(Status::invalid_argument(format!("decode error: {}", e))),
        }
    }
}

// ── RaftGrpcServer ────────────────────────────────────────────────────────────

/// Thin wrapper around the Raft tonic server. Call `serve()` to start listening.
pub struct RaftGrpcServer {
    service: RaftServiceImpl,
}

impl RaftGrpcServer {
    pub fn new(step_tx: mpsc::UnboundedSender<Message>) -> Self {
        Self {
            service: RaftServiceImpl { step_tx },
        }
    }

    pub async fn serve(self, addr: &str) -> anyhow::Result<()> {
        let addr = addr.parse()?;
        log::info!("Raft RPC server listening on {}", addr);
        tonic::transport::Server::builder()
            .add_service(raft_proto::raft_server::RaftServer::new(self.service))
            .serve(addr)
            .await?;
        Ok(())
    }
}
