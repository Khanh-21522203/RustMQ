use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use raft::eraftpb::Message;

use crate::broker::raft_network::PeerInfo;
use crate::broker::raft_transport::RaftTransport;

use super::connection::ConnectionManager;

/// Implements `RaftTransport` over SBE-encoded raw TCP connections (outbound only).
/// The inbound side is `SbeTcpServer` — construct it separately with the `step_tx`
/// from the same channel pair you pass to `RaftNode::new_with_transport`.
pub struct SbeTcpTransport {
    manager: Arc<ConnectionManager>,
}

impl SbeTcpTransport {
    pub fn new(node_id: u64, peers: Arc<RwLock<HashMap<u64, PeerInfo>>>) -> Self {
        Self {
            manager: Arc::new(ConnectionManager::new(node_id, peers)),
        }
    }
}

#[async_trait::async_trait]
impl RaftTransport for SbeTcpTransport {
    async fn send_messages(&self, msgs: Vec<Message>) {
        self.manager.send_messages(msgs).await;
    }
}
