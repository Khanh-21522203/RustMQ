use crate::api::broker::broker_client::BrokerClient;
use crate::api::broker::*;
use crate::utils::format_endpoint_addr;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::Mutex;
use tonic::transport::{Channel, Endpoint};
use tonic::{Request, Response, Status};

pub struct KafkaBrokerClient {
    client: Arc<Mutex<BrokerClient<Channel>>>,
}

#[async_trait]
pub trait KafkaBrokerClientTrait {
    async fn get_topic_metadata(&self, request: Request<TopicMetadataRequest>)
        -> Result<Response<TopicMetadataResponse>, Status>;
    async fn produce(&self, request: Request<ProduceRequest>)
        -> Result<Response<ProduceResponse>, Status>;
    async fn fetch(&self, request: Request<FetchRequest>)
        -> Result<Response<FetchResponse>, Status>;
    async fn list_offsets(&self, request: Request<ListOffsetsRequest>)
        -> Result<Response<ListOffsetsResponse>, Status>;
    async fn find_coordinator(&self, request: Request<GroupCoordinatorRequest>)
        -> Result<Response<GroupCoordinatorResponse>, Status>;
    async fn join_group(&self, request: Request<JoinGroupRequest>)
        -> Result<Response<JoinGroupResponse>, Status>;
    async fn sync_group(&self, request: Request<SyncGroupRequest>)
        -> Result<Response<SyncGroupResponse>, Status>;
    async fn heartbeat(&self, request: Request<HeartbeatRequest>)
        -> Result<Response<HeartbeatResponse>, Status>;
    async fn leave_group(&self, request: Request<LeaveGroupRequest>)
        -> Result<Response<LeaveGroupResponse>, Status>;
    async fn commit_offset(&self, request: Request<OffsetCommitRequest>)
        -> Result<Response<OffsetCommitResponse>, Status>;
    async fn fetch_offset(&self, request: Request<OffsetFetchRequest>)
        -> Result<Response<OffsetFetchResponse>, Status>;
}

impl KafkaBrokerClient {
    pub async fn new(addr: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let endpoint_addr = format_endpoint_addr(addr);
        let endpoint = Endpoint::from_shared(endpoint_addr)?;
        let channel = endpoint.connect().await?;
        let client = BrokerClient::new(channel);

        Ok(KafkaBrokerClient {
            client: Arc::new(Mutex::new(client)),
        })
    }
}

#[async_trait]
impl KafkaBrokerClientTrait for KafkaBrokerClient {
    async fn get_topic_metadata(&self, request: Request<TopicMetadataRequest>) -> Result<Response<TopicMetadataResponse>, Status> {
        todo!()
    }

    async fn produce(&self, request: Request<ProduceRequest>) -> Result<Response<ProduceResponse>, Status> {
        todo!()
    }

    async fn fetch(&self, request: Request<FetchRequest>) -> Result<Response<FetchResponse>, Status> {
        todo!()
    }

    async fn list_offsets(&self, request: Request<ListOffsetsRequest>) -> Result<Response<ListOffsetsResponse>, Status> {
        todo!()
    }

    async fn find_coordinator(&self, request: Request<GroupCoordinatorRequest>) -> Result<Response<GroupCoordinatorResponse>, Status> {
        todo!()
    }

    async fn join_group(&self, request: Request<JoinGroupRequest>) -> Result<Response<JoinGroupResponse>, Status> {
        todo!()
    }

    async fn sync_group(&self, request: Request<SyncGroupRequest>) -> Result<Response<SyncGroupResponse>, Status> {
        todo!()
    }

    async fn heartbeat(&self, request: Request<HeartbeatRequest>) -> Result<Response<HeartbeatResponse>, Status> {
        todo!()
    }

    async fn leave_group(&self, request: Request<LeaveGroupRequest>) -> Result<Response<LeaveGroupResponse>, Status> {
        todo!()
    }

    async fn commit_offset(&self, request: Request<OffsetCommitRequest>) -> Result<Response<OffsetCommitResponse>, Status> {
        todo!()
    }

    async fn fetch_offset(&self, request: Request<OffsetFetchRequest>) -> Result<Response<OffsetFetchResponse>, Status> {
        todo!()
    }
}
