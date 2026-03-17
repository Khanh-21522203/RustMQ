use crate::api::broker::{
    CreateTopicResponse, FetchResponse, GroupCoordinatorResponse, HeartbeatResponse,
    JoinGroupResponse, LeaveGroupResponse, ListOffsetsResponse, OffsetCommitResponse,
    OffsetFetchResponse, ProduceResponse, SyncGroupResponse, TopicMetadataResponse,
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
}
