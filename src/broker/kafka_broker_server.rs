use std::error::Error;
use std::time::Duration;
use async_trait::async_trait;
use tokio::sync::mpsc;
use tonic::{Request, Response, Status};
use tonic::transport::Server;
use crate::api::broker::*;
use crate::api::requests::BrokerGrpcRequest;
use crate::api::responses::BrokerGrpcResponse;

pub struct KafkaBrokerServer {
    rpc_send_channel: mpsc::Sender<(BrokerGrpcRequest, mpsc::Sender<BrokerGrpcResponse>)>
}

impl KafkaBrokerServer {
    pub fn new(rpc_send_channel: mpsc::Sender<(BrokerGrpcRequest, mpsc::Sender<BrokerGrpcResponse>)>) -> Self{
        KafkaBrokerServer{
            rpc_send_channel
        }
    }

    pub async fn run(self, addr: &str) -> Result<(), Box<dyn Error>> {
        let endpoint_addr = addr.to_string().parse()?;

        tokio::spawn( async move {
            Server::builder()
                .add_service(broker_server::BrokerServer::new(self))
                .serve(endpoint_addr)
                .await.expect("unable to start up grpc server");
        });

        tokio::time::sleep(Duration::from_millis(100)).await;

        log::info!("grpc server started at {}", endpoint_addr);
        Ok(())
    }
}
#[async_trait]
impl broker_server::Broker for KafkaBrokerServer {
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
