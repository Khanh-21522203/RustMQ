use crate::api::requests::BrokerGrpcRequest;
use crate::api::responses::BrokerGrpcResponse;
use crate::broker::core::BrokerCore;
use crate::broker::storage::BrokerStorage;

pub struct BrokerRpcRouter<'a, S: BrokerStorage> {
    core: &'a BrokerCore<S>,
}

impl<'a, S: BrokerStorage + 'static> BrokerRpcRouter<'a, S> {
    pub fn new(core: &'a BrokerCore<S>) -> Self {
        Self { core }
    }

    pub async fn dispatch(&self, request: BrokerGrpcRequest) -> BrokerGrpcResponse {
        match request {
            BrokerGrpcRequest::GetTopicMetadata(req) => {
                BrokerGrpcResponse::GetTopicMetadata(self.core.handle_get_topic_metadata(req).await)
            }
            BrokerGrpcRequest::Produce(req) => {
                BrokerGrpcResponse::Produce(self.core.handle_produce(req).await)
            }
            BrokerGrpcRequest::Fetch(req) => {
                BrokerGrpcResponse::Fetch(self.core.handle_fetch(req).await)
            }
            BrokerGrpcRequest::ListOffsets(req) => {
                BrokerGrpcResponse::ListOffsets(self.core.handle_list_offsets(req).await)
            }
            BrokerGrpcRequest::FindCoordinator(req) => {
                BrokerGrpcResponse::FindCoordinator(self.core.handle_find_coordinator(req).await)
            }
            BrokerGrpcRequest::JoinGroup(req) => {
                BrokerGrpcResponse::JoinGroup(self.core.handle_join_group(req).await)
            }
            BrokerGrpcRequest::SyncGroup(req) => {
                BrokerGrpcResponse::SyncGroup(self.core.handle_sync_group(req).await)
            }
            BrokerGrpcRequest::Heartbeat(req) => {
                BrokerGrpcResponse::Heartbeat(self.core.handle_heartbeat(req).await)
            }
            BrokerGrpcRequest::LeaveGroup(req) => {
                BrokerGrpcResponse::LeaveGroup(self.core.handle_leave_group(req).await)
            }
            BrokerGrpcRequest::CommitOffset(req) => {
                BrokerGrpcResponse::CommitOffset(self.core.handle_commit_offset(req).await)
            }
            BrokerGrpcRequest::FetchOffset(req) => {
                BrokerGrpcResponse::FetchOffset(self.core.handle_fetch_offset(req).await)
            }
            BrokerGrpcRequest::CreateTopic(req) => {
                BrokerGrpcResponse::CreateTopic(self.core.handle_create_topic(req).await)
            }
            BrokerGrpcRequest::AddNode(req) => {
                BrokerGrpcResponse::AddNode(self.core.handle_add_node(req).await)
            }
            BrokerGrpcRequest::RemoveNode(req) => {
                BrokerGrpcResponse::RemoveNode(self.core.handle_remove_node(req).await)
            }
        }
    }
}
