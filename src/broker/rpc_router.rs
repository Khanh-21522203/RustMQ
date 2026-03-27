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
                log::debug!("recv GetTopicMetadata topics={:?}", req.topics);
                BrokerGrpcResponse::GetTopicMetadata(self.core.handle_get_topic_metadata(req).await)
            }
            BrokerGrpcRequest::Produce(req) => {
                for t in &req.topics {
                    let record_count: usize = t.partitions.iter().map(|p| p.records.len()).sum();
                    log::debug!("recv Produce topic={} records={}", t.topic_name, record_count);
                }
                BrokerGrpcResponse::Produce(self.core.handle_produce(req).await)
            }
            BrokerGrpcRequest::Fetch(req) => {
                for t in &req.topics {
                    let partitions: Vec<i32> = t.partitions.iter().map(|p| p.partition).collect();
                    log::debug!("recv Fetch topic={} partitions={:?}", t.topic_name, partitions);
                }
                BrokerGrpcResponse::Fetch(self.core.handle_fetch(req).await)
            }
            BrokerGrpcRequest::ListOffsets(req) => {
                log::debug!("recv ListOffsets");
                BrokerGrpcResponse::ListOffsets(self.core.handle_list_offsets(req).await)
            }
            BrokerGrpcRequest::FindCoordinator(req) => {
                log::debug!("recv FindCoordinator group={}", req.group_id);
                BrokerGrpcResponse::FindCoordinator(self.core.handle_find_coordinator(req).await)
            }
            BrokerGrpcRequest::JoinGroup(req) => {
                log::info!("recv JoinGroup group={} member={}", req.group_id, req.member_id);
                BrokerGrpcResponse::JoinGroup(self.core.handle_join_group(req).await)
            }
            BrokerGrpcRequest::SyncGroup(req) => {
                log::debug!("recv SyncGroup group={} member={}", req.group_id, req.member_id);
                BrokerGrpcResponse::SyncGroup(self.core.handle_sync_group(req).await)
            }
            BrokerGrpcRequest::Heartbeat(req) => {
                log::debug!("recv Heartbeat group={} member={}", req.group_id, req.member_id);
                BrokerGrpcResponse::Heartbeat(self.core.handle_heartbeat(req).await)
            }
            BrokerGrpcRequest::LeaveGroup(req) => {
                log::info!("recv LeaveGroup group={} member={}", req.group_id, req.member_id);
                BrokerGrpcResponse::LeaveGroup(self.core.handle_leave_group(req).await)
            }
            BrokerGrpcRequest::CommitOffset(req) => {
                log::debug!("recv CommitOffset group={}", req.consumer_group_id);
                BrokerGrpcResponse::CommitOffset(self.core.handle_commit_offset(req).await)
            }
            BrokerGrpcRequest::FetchOffset(req) => {
                log::debug!("recv FetchOffset group={}", req.consumer_group_id);
                BrokerGrpcResponse::FetchOffset(self.core.handle_fetch_offset(req).await)
            }
            BrokerGrpcRequest::CreateTopic(req) => {
                log::info!("recv CreateTopic topic={} partitions={}", req.topic_name, req.num_partitions);
                BrokerGrpcResponse::CreateTopic(self.core.handle_create_topic(req).await)
            }
            BrokerGrpcRequest::AddNode(req) => {
                log::info!("recv AddNode node_id={} api={}", req.node_id, req.api_addr);
                BrokerGrpcResponse::AddNode(self.core.handle_add_node(req).await)
            }
            BrokerGrpcRequest::RemoveNode(req) => {
                log::info!("recv RemoveNode node_id={}", req.node_id);
                BrokerGrpcResponse::RemoveNode(self.core.handle_remove_node(req).await)
            }
        }
    }
}
