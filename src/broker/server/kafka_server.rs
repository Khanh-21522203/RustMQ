use crate::api::broker::*;
use crate::api::requests::BrokerGrpcRequest;
use crate::api::responses::BrokerGrpcResponse;
use async_trait::async_trait;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tonic::transport::Server;
use tonic::{Request, Response, Status};

pub struct KafkaBrokerServer {
    rpc_send_channel: mpsc::Sender<(BrokerGrpcRequest, oneshot::Sender<BrokerGrpcResponse>)>,
    membership_api_token: Option<Arc<str>>,
}

impl KafkaBrokerServer {
    pub fn new(
        rpc_send_channel: mpsc::Sender<(BrokerGrpcRequest, oneshot::Sender<BrokerGrpcResponse>)>,
        membership_api_token: Option<String>,
    ) -> Self {
        KafkaBrokerServer {
            rpc_send_channel,
            membership_api_token: membership_api_token.map(Arc::<str>::from),
        }
    }

    pub async fn serve(self, addr: SocketAddr) -> Result<(), tonic::transport::Error> {
        log::info!("gRPC server listening on {}", addr);
        Server::builder()
            .add_service(broker_server::BrokerServer::new(self))
            .serve(addr)
            .await
    }

    pub async fn run(self, addr: &str) -> Result<(), Box<dyn std::error::Error>> {
        let endpoint_addr: SocketAddr = addr.parse()?;
        self.serve(endpoint_addr).await?;
        Ok(())
    }

    fn enforce_membership_auth<T>(&self, request: &Request<T>) -> Result<(), Status> {
        let Some(expected) = &self.membership_api_token else {
            return Ok(());
        };

        let value = request
            .metadata()
            .get("x-rustmq-admin-token")
            .ok_or_else(|| Status::unauthenticated("missing x-rustmq-admin-token"))?;
        let provided = value
            .to_str()
            .map_err(|_| Status::unauthenticated("invalid x-rustmq-admin-token encoding"))?;
        if provided != expected.as_ref() {
            return Err(Status::permission_denied(
                "invalid token for membership operation",
            ));
        }
        Ok(())
    }
}

/// Helper: send a request and receive a response via oneshot channel.
macro_rules! dispatch {
    ($self:expr, $variant:ident, $request:expr) => {{
        let (tx, rx) = oneshot::channel();
        if let Err(e) = $self
            .rpc_send_channel
            .send((BrokerGrpcRequest::$variant($request.into_inner()), tx))
            .await
        {
            return Err(Status::internal(format!("broker unavailable: {}", e)));
        }
        match rx.await {
            Ok(BrokerGrpcResponse::$variant(res)) => Ok(Response::new(res)),
            Ok(_) => Err(Status::internal("unexpected response variant")),
            Err(_) => Err(Status::internal("broker did not respond")),
        }
    }};
}

#[async_trait]
impl broker_server::Broker for KafkaBrokerServer {
    async fn get_topic_metadata(
        &self,
        request: Request<TopicMetadataRequest>,
    ) -> Result<Response<TopicMetadataResponse>, Status> {
        dispatch!(self, GetTopicMetadata, request)
    }

    async fn produce(
        &self,
        request: Request<ProduceRequest>,
    ) -> Result<Response<ProduceResponse>, Status> {
        dispatch!(self, Produce, request)
    }

    async fn fetch(
        &self,
        request: Request<FetchRequest>,
    ) -> Result<Response<FetchResponse>, Status> {
        dispatch!(self, Fetch, request)
    }

    async fn list_offsets(
        &self,
        request: Request<ListOffsetsRequest>,
    ) -> Result<Response<ListOffsetsResponse>, Status> {
        dispatch!(self, ListOffsets, request)
    }

    async fn find_coordinator(
        &self,
        request: Request<GroupCoordinatorRequest>,
    ) -> Result<Response<GroupCoordinatorResponse>, Status> {
        dispatch!(self, FindCoordinator, request)
    }

    async fn join_group(
        &self,
        request: Request<JoinGroupRequest>,
    ) -> Result<Response<JoinGroupResponse>, Status> {
        dispatch!(self, JoinGroup, request)
    }

    async fn sync_group(
        &self,
        request: Request<SyncGroupRequest>,
    ) -> Result<Response<SyncGroupResponse>, Status> {
        dispatch!(self, SyncGroup, request)
    }

    async fn heartbeat(
        &self,
        request: Request<HeartbeatRequest>,
    ) -> Result<Response<HeartbeatResponse>, Status> {
        dispatch!(self, Heartbeat, request)
    }

    async fn leave_group(
        &self,
        request: Request<LeaveGroupRequest>,
    ) -> Result<Response<LeaveGroupResponse>, Status> {
        dispatch!(self, LeaveGroup, request)
    }

    async fn commit_offset(
        &self,
        request: Request<OffsetCommitRequest>,
    ) -> Result<Response<OffsetCommitResponse>, Status> {
        dispatch!(self, CommitOffset, request)
    }

    async fn fetch_offset(
        &self,
        request: Request<OffsetFetchRequest>,
    ) -> Result<Response<OffsetFetchResponse>, Status> {
        dispatch!(self, FetchOffset, request)
    }

    async fn create_topic(
        &self,
        request: Request<CreateTopicRequest>,
    ) -> Result<Response<CreateTopicResponse>, Status> {
        dispatch!(self, CreateTopic, request)
    }

    async fn add_node(
        &self,
        request: Request<AddNodeRequest>,
    ) -> Result<Response<AddNodeResponse>, Status> {
        self.enforce_membership_auth(&request)?;
        dispatch!(self, AddNode, request)
    }

    async fn remove_node(
        &self,
        request: Request<RemoveNodeRequest>,
    ) -> Result<Response<RemoveNodeResponse>, Status> {
        self.enforce_membership_auth(&request)?;
        dispatch!(self, RemoveNode, request)
    }
}
