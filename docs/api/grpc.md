# gRPC API Reference

Rust-MQ exposes a gRPC API defined in `src/api/kafka.proto`.

## Service Definition

```protobuf
service Broker {
  rpc GetTopicMetadata(TopicMetadataRequest) returns (TopicMetadataResponse);
  rpc Produce(ProduceRequest) returns (ProduceResponse);
  rpc Fetch(FetchRequest) returns (FetchResponse);
  rpc ListOffsets(ListOffsetsRequest) returns (ListOffsetsResponse);
  rpc FindCoordinator(GroupCoordinatorRequest) returns (GroupCoordinatorResponse);
  rpc JoinGroup(JoinGroupRequest) returns (JoinGroupResponse);
  rpc SyncGroup(SyncGroupRequest) returns (SyncGroupResponse);
  rpc Heartbeat(HeartbeatRequest) returns (HeartbeatResponse);
  rpc LeaveGroup(LeaveGroupRequest) returns (LeaveGroupResponse);
  rpc CommitOffset(OffsetCommitRequest) returns (OffsetCommitResponse);
  rpc FetchOffset(OffsetFetchRequest) returns (OffsetFetchResponse);
  rpc CreateTopic(CreateTopicRequest) returns (CreateTopicResponse);
}
```

## Key Request Types

- `TopicMetadataRequest`: list metadata for selected topics (or all topics when empty).
- `ProduceRequest`: write records by `topic_name` and `partition`.
- `FetchRequest`: read records from `(topic, partition, fetch_offset)`.
- `ListOffsetsRequest`: query earliest (`time = -2`) or latest (`time = -1`) offset.
- `GroupCoordinatorRequest`: find coordinator for `group_id`.
- `OffsetCommitRequest` / `OffsetFetchRequest`: commit and fetch consumer-group offsets.
- `CreateTopicRequest`: create a topic with `num_partitions`.

Refer to `src/api/kafka.proto` for full nested message schemas.

## Error Codes (Current Runtime Behavior)

Error code mapping is method-specific in broker handlers:

- `0`: success
- `1`: generic operation failure
- `6`: not leader for partition (`ProduceResponse` includes `leader_addr`)
- `27`: rebalance in progress (consumer-group heartbeat/sync paths)

## Using the API from Other Languages

```bash
# Go
protoc --go_out=. --go-grpc_out=. src/api/kafka.proto

# Python
python -m grpc_tools.protoc -I src/api --python_out=. --grpc_python_out=. kafka.proto
```
