use crate::api::broker::{
    AddNodeResponse, CreateTopicResponse, FetchResponse, GroupCoordinatorResponse,
    HeartbeatResponse, JoinGroupResponse, LeaveGroupResponse, ListOffsetsResponse,
    OffsetCommitResponse, OffsetFetchResponse, ProduceResponse, RemoveNodeResponse,
    SyncGroupResponse, TopicMetadataResponse,
};

pub enum BrokerGrpcResponse {
    // Topic Management
    GetTopicMetadata(TopicMetadataResponse),
    CreateTopic(CreateTopicResponse),

    // Producer Operations
    Produce(ProduceResponse),

    // Consumer Operations
    Fetch(FetchResponse),
    ListOffsets(ListOffsetsResponse),

    // Consumer Group Coordination
    FindCoordinator(GroupCoordinatorResponse),
    JoinGroup(JoinGroupResponse),
    SyncGroup(SyncGroupResponse),
    Heartbeat(HeartbeatResponse),
    LeaveGroup(LeaveGroupResponse),

    // Offset Management
    CommitOffset(OffsetCommitResponse),
    FetchOffset(OffsetFetchResponse),

    // Cluster Membership
    AddNode(AddNodeResponse),
    RemoveNode(RemoveNodeResponse),
}
