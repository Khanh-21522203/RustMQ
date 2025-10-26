use crate::api::broker::{
    FetchRequest, GroupCoordinatorRequest, HeartbeatRequest, JoinGroupRequest, LeaveGroupRequest,
    ListOffsetsRequest, OffsetCommitRequest, OffsetFetchRequest, ProduceRequest, SyncGroupRequest,
    TopicMetadataRequest,
};
pub enum BrokerGrpcRequest {
    // Topic Management
    GetTopicMetadata(TopicMetadataRequest),

    // Producer Operations
    Produce(ProduceRequest),

    // Consumer Operations
    Fetch(FetchRequest),
    ListOffsets(ListOffsetsRequest),

    // Consumer Group Coordination
    FindCoordinator(GroupCoordinatorRequest),
    JoinGroup(JoinGroupRequest),
    SyncGroup(SyncGroupRequest),
    Heartbeat(HeartbeatRequest),
    LeaveGroup(LeaveGroupRequest),

    // Offset Management
    CommitOffset(OffsetCommitRequest),
    FetchOffset(OffsetFetchRequest),
}
