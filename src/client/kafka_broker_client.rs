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
        -> Result<TopicMetadataResponse, Status>;
    async fn produce(&self, request: Request<ProduceRequest>)
        -> Result<ProduceResponse, Status>;
    async fn fetch(&self, request: Request<FetchRequest>)
        -> Result<FetchResponse, Status>;
    async fn list_offsets(&self, request: Request<ListOffsetsRequest>)
        -> Result<ListOffsetsResponse, Status>;
    async fn find_coordinator(&self, request: Request<GroupCoordinatorRequest>)
        -> Result<GroupCoordinatorResponse, Status>;
    async fn join_group(&self, request: Request<JoinGroupRequest>)
        -> Result<JoinGroupResponse, Status>;
    async fn sync_group(&self, request: Request<SyncGroupRequest>)
        -> Result<SyncGroupResponse, Status>;
    async fn heartbeat(&self, request: Request<HeartbeatRequest>)
        -> Result<HeartbeatResponse, Status>;
    async fn leave_group(&self, request: Request<LeaveGroupRequest>)
        -> Result<LeaveGroupResponse, Status>;
    async fn commit_offset(&self, request: Request<OffsetCommitRequest>)
        -> Result<OffsetCommitResponse, Status>;
    async fn fetch_offset(&self, request: Request<OffsetFetchRequest>)
        -> Result<OffsetFetchResponse, Status>;
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
    async fn get_topic_metadata(&self, request: Request<TopicMetadataRequest>) -> Result<TopicMetadataResponse, Status> {
        let mut client = self.client.lock().await;
        let response = client.get_topic_metadata(request).await?;
        Ok(response.into_inner())
    }

    async fn produce(&self, request: Request<ProduceRequest>) -> Result<ProduceResponse, Status> {
        let mut client = self.client.lock().await;
        let response = client.produce(request).await?;
        Ok(response.into_inner())
    }

    async fn fetch(&self, request: Request<FetchRequest>) -> Result<FetchResponse, Status> {
        let mut client = self.client.lock().await;
        let response = client.fetch(request).await?;
        Ok(response.into_inner())
    }

    async fn list_offsets(&self, request: Request<ListOffsetsRequest>) -> Result<ListOffsetsResponse, Status> {
        let mut client = self.client.lock().await;
        let response = client.list_offsets(request).await?;
        Ok(response.into_inner())
    }

    async fn find_coordinator(&self, request: Request<GroupCoordinatorRequest>) -> Result<GroupCoordinatorResponse, Status> {
        let mut client = self.client.lock().await;
        let response = client.find_coordinator(request).await?;
        Ok(response.into_inner())
    }

    async fn join_group(&self, request: Request<JoinGroupRequest>) -> Result<JoinGroupResponse, Status> {
        let mut client = self.client.lock().await;
        let response = client.join_group(request).await?;
        Ok(response.into_inner())
    }

    async fn sync_group(&self, request: Request<SyncGroupRequest>) -> Result<SyncGroupResponse, Status> {
        let mut client = self.client.lock().await;
        let response = client.sync_group(request).await?;
        Ok(response.into_inner())
    }

    async fn heartbeat(&self, request: Request<HeartbeatRequest>) -> Result<HeartbeatResponse, Status> {
        let mut client = self.client.lock().await;
        let response = client.heartbeat(request).await?;
        Ok(response.into_inner())
    }

    async fn leave_group(&self, request: Request<LeaveGroupRequest>) -> Result<LeaveGroupResponse, Status> {
        let mut client = self.client.lock().await;
        let response = client.leave_group(request).await?;
        Ok(response.into_inner())
    }

    async fn commit_offset(&self, request: Request<OffsetCommitRequest>) -> Result<OffsetCommitResponse, Status> {
        let mut client = self.client.lock().await;
        let response = client.commit_offset(request).await?;
        Ok(response.into_inner())
    }

    async fn fetch_offset(&self, request: Request<OffsetFetchRequest>) -> Result<OffsetFetchResponse, Status> {
        let mut client = self.client.lock().await;
        let response = client.fetch_offset(request).await?;
        Ok(response.into_inner())
    }
}
