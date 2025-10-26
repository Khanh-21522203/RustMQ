use tokio::sync::mpsc;
use crate::api::requests::BrokerGrpcRequest;
use crate::api::responses::BrokerGrpcResponse;
use crate::api::broker::*;

pub struct BrokerCore {
    rpc_receive_channel: mpsc::Receiver<(BrokerGrpcRequest, mpsc::Sender<BrokerGrpcResponse>)>,
}

impl BrokerCore {
    pub fn new(rpc_receive_channel: mpsc::Receiver<(BrokerGrpcRequest, mpsc::Sender<BrokerGrpcResponse>)>) -> Self {
        Self {
            rpc_receive_channel,
        }
    }

    async fn handle_rpc_event(&mut self, rpc_event: Option<(BrokerGrpcRequest, mpsc::Sender<BrokerGrpcResponse>)>) {
        match rpc_event {
            Some((request, response_sender)) => {
                let result = match request {
                    BrokerGrpcRequest::GetTopicMetadata(request) => {
                        let response = self.handle_get_topic_metadata(request).await; 
                        response_sender.send(BrokerGrpcResponse::GetTopicMetadata(response)).await
                    },
                    BrokerGrpcRequest::Produce(request) => {
                        let response = self.handle_produce(request).await;
                        response_sender.send(BrokerGrpcResponse::Produce(response)).await
                    },
                    BrokerGrpcRequest::Fetch(request) => {
                        let response = self.handle_fetch(request).await;
                        response_sender.send(BrokerGrpcResponse::Fetch(response)).await
                    },
                    BrokerGrpcRequest::ListOffsets(request) => {
                        let response = self.handle_list_offsets(request).await;
                        response_sender.send(BrokerGrpcResponse::ListOffsets(response)).await
                    },
                    BrokerGrpcRequest::FindCoordinator(request) => {
                        let response = self.handle_find_coordinator(request).await;
                        response_sender.send(BrokerGrpcResponse::FindCoordinator(response)).await
                    },
                    BrokerGrpcRequest::JoinGroup(request) => {
                        let response = self.handle_join_group(request).await;
                        response_sender.send(BrokerGrpcResponse::JoinGroup(response)).await
                    },
                    BrokerGrpcRequest::SyncGroup(request) => {
                        let response = self.handle_sync_group(request).await;
                        response_sender.send(BrokerGrpcResponse::SyncGroup(response)).await
                    },
                    BrokerGrpcRequest::Heartbeat(request) => {
                        let response = self.handle_heartbeat(request).await;
                        response_sender.send(BrokerGrpcResponse::Heartbeat(response)).await
                    },
                    BrokerGrpcRequest::LeaveGroup(request) => {
                        let response = self.handle_leave_group(request).await;
                        response_sender.send(BrokerGrpcResponse::LeaveGroup(response)).await
                    },
                    BrokerGrpcRequest::CommitOffset(request) => {
                        let response = self.handle_commit_offset(request).await;
                        response_sender.send(BrokerGrpcResponse::CommitOffset(response)).await
                    },
                    BrokerGrpcRequest::FetchOffset(request) => {
                        let response = self.handle_fetch_offset(request).await;
                        response_sender.send(BrokerGrpcResponse::FetchOffset(response)).await
                    },
                };
            }
            None => {
                log::error!("Failed to receive RPC event");
            }
        }
    }
    async fn handle_get_topic_metadata(&mut self, args: TopicMetadataRequest) -> TopicMetadataResponse {}
    async fn handle_produce(&mut self, args: ProduceRequest) -> ProduceResponse {}
    async fn handle_fetch(&mut self, args: FetchRequest) -> FetchResponse {}
    async fn handle_list_offsets(&mut self, args: ListOffsetsRequest) -> ListOffsetsResponse {}
    async fn handle_find_coordinator(&mut self, args: GroupCoordinatorRequest) -> GroupCoordinatorResponse {}
    async fn handle_join_group(&mut self, args: JoinGroupRequest) -> JoinGroupResponse {}
    async fn handle_sync_group(&mut self, args: SyncGroupRequest) -> SyncGroupResponse {}
    async fn handle_heartbeat(&mut self, args: HeartbeatRequest) -> HeartbeatResponse {}
    async fn handle_leave_group(&mut self, args: LeaveGroupRequest) -> LeaveGroupResponse {}
    async fn handle_commit_offset(&mut self, args: OffsetCommitRequest) -> OffsetCommitResponse {}
    async fn handle_fetch_offset(&mut self, args: OffsetFetchRequest) -> OffsetFetchResponse {}
    
}
