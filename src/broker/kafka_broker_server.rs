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
        let (response_sender, mut response_receiver) = mpsc::channel(1);
        match self.rpc_send_channel.send((BrokerGrpcRequest::GetTopicMetadata(request.into_inner()), response_sender)).await {
            Ok(_) => {
                match response_receiver.recv().await {
                    Some(BrokerGrpcResponse::GetTopicMetadata(res)) => Ok(Response::new(res)),
                    _ => Err(Status::internal("failed to get topic metadata"))
                }
            },
            Err(e) => {
                log::error!("failed to get topic metadata: {}", e);
                Err(Status::internal(format!("failed to get topic metadata: {}", e)))
            }
        }
    }

    async fn produce(&self, request: Request<ProduceRequest>) -> Result<Response<ProduceResponse>, Status> {
        let (response_sender, mut response_receiver) = mpsc::channel(1);
        match self.rpc_send_channel.send((BrokerGrpcRequest::Produce(request.into_inner()), response_sender)).await {
            Ok(_) => {
                match response_receiver.recv().await {
                    Some(BrokerGrpcResponse::Produce(res)) => Ok(Response::new(res)),
                    _ => Err(Status::internal("failed to produce"))
                }
            },
            Err(e) => {
                log::error!("failed to produce: {}", e);
                Err(Status::internal(format!("failed to produce: {}", e)))
            }
        }
    }

    async fn fetch(&self, request: Request<FetchRequest>) -> Result<Response<FetchResponse>, Status> {
        let (response_sender, mut response_receiver) = mpsc::channel(1);
        match self.rpc_send_channel.send((BrokerGrpcRequest::Fetch(request.into_inner()), response_sender)).await {
            Ok(_) => {
                match response_receiver.recv().await {
                    Some(BrokerGrpcResponse::Fetch(res)) => Ok(Response::new(res)),
                    _ => Err(Status::internal("failed to fetch"))
                }
            },
            Err(e) => {
                log::error!("failed to fetch: {}", e);
                Err(Status::internal(format!("failed to fetch: {}", e)))
            }
        }
    }

    async fn list_offsets(&self, request: Request<ListOffsetsRequest>) -> Result<Response<ListOffsetsResponse>, Status> {
        let (response_sender, mut response_receiver) = mpsc::channel(1);
        match self.rpc_send_channel.send((BrokerGrpcRequest::ListOffsets(request.into_inner()), response_sender)).await {
            Ok(_) => {
                match response_receiver.recv().await {
                    Some(BrokerGrpcResponse::ListOffsets(res)) => Ok(Response::new(res)),
                    _ => Err(Status::internal("failed to list offsets"))
                }
            },
            Err(e) => {
                log::error!("failed to list offsets: {}", e);
                Err(Status::internal(format!("failed to list offsets: {}", e)))
            }
        }
    }

    async fn find_coordinator(&self, request: Request<GroupCoordinatorRequest>) -> Result<Response<GroupCoordinatorResponse>, Status> {
        let (response_sender, mut response_receiver) = mpsc::channel(1);
        match self.rpc_send_channel.send((BrokerGrpcRequest::FindCoordinator(request.into_inner()), response_sender)).await {
            Ok(_) => {
                match response_receiver.recv().await {
                    Some(BrokerGrpcResponse::FindCoordinator(res)) => Ok(Response::new(res)),
                    _ => Err(Status::internal("failed to find coordinator"))
                }
            },
            Err(e) => {
                log::error!("failed to find coordinator: {}", e);
                Err(Status::internal(format!("failed to find coordinator: {}", e)))
            }
        }
    }

    async fn join_group(&self, request: Request<JoinGroupRequest>) -> Result<Response<JoinGroupResponse>, Status> {
        let (response_sender, mut response_receiver) = mpsc::channel(1);
        match self.rpc_send_channel.send((BrokerGrpcRequest::JoinGroup(request.into_inner()), response_sender)).await {
            Ok(_) => {
                match response_receiver.recv().await {
                    Some(BrokerGrpcResponse::JoinGroup(res)) => Ok(Response::new(res)),
                    _ => Err(Status::internal("failed to join group"))
                }
            },
            Err(e) => {
                log::error!("failed to join group: {}", e);
                Err(Status::internal(format!("failed to join group: {}", e)))
            }
        }
    }

    async fn sync_group(&self, request: Request<SyncGroupRequest>) -> Result<Response<SyncGroupResponse>, Status> {
        let (response_sender, mut response_receiver) = mpsc::channel(1);
        match self.rpc_send_channel.send((BrokerGrpcRequest::SyncGroup(request.into_inner()), response_sender)).await {
            Ok(_) => {
                match response_receiver.recv().await {
                    Some(BrokerGrpcResponse::SyncGroup(res)) => Ok(Response::new(res)),
                    _ => Err(Status::internal("failed to sync group"))
                }
            },
            Err(e) => {
                log::error!("failed to sync group: {}", e);
                Err(Status::internal(format!("failed to sync group: {}", e)))
            }
        }
    }

    async fn heartbeat(&self, request: Request<HeartbeatRequest>) -> Result<Response<HeartbeatResponse>, Status> {
        let (response_sender, mut response_receiver) = mpsc::channel(1);
        match self.rpc_send_channel.send((BrokerGrpcRequest::Heartbeat(request.into_inner()), response_sender)).await {
            Ok(_) => {
                match response_receiver.recv().await {
                    Some(BrokerGrpcResponse::Heartbeat(res)) => Ok(Response::new(res)),
                    _ => Err(Status::internal("failed to heartbeat"))
                }
            },
            Err(e) => {
                log::error!("failed to heartbeat: {}", e);
                Err(Status::internal(format!("failed to heartbeat: {}", e)))
            }
        }
    }

    async fn leave_group(&self, request: Request<LeaveGroupRequest>) -> Result<Response<LeaveGroupResponse>, Status> {
        let (response_sender, mut response_receiver) = mpsc::channel(1);
        match self.rpc_send_channel.send((BrokerGrpcRequest::LeaveGroup(request.into_inner()), response_sender)).await {
            Ok(_) => {
                match response_receiver.recv().await {
                    Some(BrokerGrpcResponse::LeaveGroup(res)) => Ok(Response::new(res)),
                    _ => Err(Status::internal("failed to leave group"))
                }
            },
            Err(e) => {
                log::error!("failed to leave group: {}", e);
                Err(Status::internal(format!("failed to leave group: {}", e)))
            }
        }
    }

    async fn commit_offset(&self, request: Request<OffsetCommitRequest>) -> Result<Response<OffsetCommitResponse>, Status> {
        let (response_sender, mut response_receiver) = mpsc::channel(1);
        match self.rpc_send_channel.send((BrokerGrpcRequest::CommitOffset(request.into_inner()), response_sender)).await {
            Ok(_) => {
                match response_receiver.recv().await {
                    Some(BrokerGrpcResponse::CommitOffset(res)) => Ok(Response::new(res)),
                    _ => Err(Status::internal("failed to commit offset"))
                }
            },
            Err(e) => {
                log::error!("failed to commit offset: {}", e);
                Err(Status::internal(format!("failed to commit offset: {}", e)))
            }
        }
    }

    async fn fetch_offset(&self, request: Request<OffsetFetchRequest>) -> Result<Response<OffsetFetchResponse>, Status> {
        let (response_sender, mut response_receiver) = mpsc::channel(1);
        match self.rpc_send_channel.send((BrokerGrpcRequest::FetchOffset(request.into_inner()), response_sender)).await {
            Ok(_) => {
                match response_receiver.recv().await {
                    Some(BrokerGrpcResponse::FetchOffset(res)) => Ok(Response::new(res)),
                    _ => Err(Status::internal("failed to fetch offset"))
                }
            },
            Err(e) => {
                log::error!("failed to fetch offset: {}", e);
                Err(Status::internal(format!("failed to fetch offset: {}", e)))
            }
        }
    }
}
