use async_trait::async_trait;
use raft::eraftpb::Message;

/// Abstraction over inter-broker Raft message transport.
/// The server (inbound) side is handled by a paired type that pushes decoded
/// messages into the `step_tx` channel owned by `RaftNode`.
#[async_trait]
pub trait RaftTransport: Send + Sync + 'static {
    /// Send outbound Raft messages.  Messages to self.node_id must be dropped.
    async fn send_messages(&self, msgs: Vec<Message>);

    /// Return the client-facing API address of a peer (for leader-redirect hints).
    fn peer_api_addr(&self, node_id: u64) -> Option<String>;
}
