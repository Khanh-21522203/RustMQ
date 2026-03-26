use crate::api::broker::{
    AddNodeRequest, CreateTopicRequest, FetchRequest, GroupCoordinatorRequest, HeartbeatRequest,
    JoinGroupRequest, LeaveGroupRequest, ListOffsetsRequest, OffsetCommitRequest,
    OffsetFetchRequest, ProduceRequest, RemoveNodeRequest, SyncGroupRequest, TopicMetadataRequest,
};
pub enum BrokerGrpcRequest {
    // Topic Management
    GetTopicMetadata(TopicMetadataRequest),
    CreateTopic(CreateTopicRequest),

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

    // Cluster Membership
    AddNode(AddNodeRequest),
    RemoveNode(RemoveNodeRequest),
}
