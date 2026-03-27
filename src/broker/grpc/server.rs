use tokio::sync::mpsc;
use tonic::{Request, Response, Status};
use prost_raft::Message as RaftProstMessage;
use raft::eraftpb::Message;

use super::transport::raft_proto;
use raft_proto::raft_server::Raft;
use raft_proto::{RaftMessage, RaftReply};

// ── Inbound: receive Raft messages from peers ─────────────────────────────────

struct RaftServiceImpl {
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
                if self.step_tx.send(msg).is_err() {
                    log::warn!("Dropped inbound raft message because step channel is closed");
                }
                Ok(Response::new(RaftReply::default()))
            }
            Err(e) => {
                log::warn!("Failed to decode inbound raft message: {}", e);
                Err(Status::invalid_argument(format!("decode error: {}", e)))
            }
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
