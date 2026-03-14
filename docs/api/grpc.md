# gRPC API Reference

Rust-MQ exposes a gRPC API modeled after the Kafka wire protocol. The protobuf definitions are in `src/api/kafka.proto`.

## Service Definition

```protobuf
service Broker {
  rpc GetTopicMetadata  (MetadataRequest)       returns (MetadataResponse);
  rpc Produce           (ProduceRequest)         returns (ProduceResponse);
  rpc Fetch             (FetchRequest)            returns (FetchResponse);
  rpc ListOffsets       (ListOffsetsRequest)     returns (ListOffsetsResponse);
  rpc FindCoordinator   (FindCoordinatorRequest) returns (FindCoordinatorResponse);
  rpc JoinGroup         (JoinGroupRequest)       returns (JoinGroupResponse);
  rpc SyncGroup         (SyncGroupRequest)       returns (SyncGroupResponse);
  rpc Heartbeat         (HeartbeatRequest)       returns (HeartbeatResponse);
  rpc LeaveGroup        (LeaveGroupRequest)      returns (LeaveGroupResponse);
  rpc CommitOffset      (OffsetCommitRequest)    returns (OffsetCommitResponse);
  rpc FetchOffset       (OffsetFetchRequest)     returns (OffsetFetchResponse);
}
```

## Error Codes

All responses include an `error_code` field:

| Code | Name | Description |
|---|---|---|
| 0 | `NoError` | Success |
| 1 | `OffsetOutOfRange` | Requested offset does not exist in the partition |
| 2 | `CorruptMessage` | Message data is malformed |
| 3 | `UnknownTopicOrPartition` | Topic or partition does not exist |
| 5 | `LeaderNotAvailable` | No leader is currently elected |
| 6 | `NotLeaderForPartition` | This node is not the leader for the requested partition |
| 16 | `NotCoordinatorForGroup` | This node is not the group coordinator |
| 22 | `IllegalGeneration` | Consumer group generation ID mismatch |
| 25 | `UnknownMemberId` | Consumer group member ID not recognized |
| 35 | `GroupLoadInProgress` | Group metadata is being loaded |

---

## Methods

### GetTopicMetadata

Returns partition count and leader information for one or more topics.

**Request**
```protobuf
message MetadataRequest {
  repeated string topics = 1;  // Empty = return metadata for all topics
}
```

**Response**
```protobuf
message MetadataResponse {
  repeated TopicMetadata topics = 1;
}

message TopicMetadata {
  int32 error_code = 1;
  string name = 2;
  repeated PartitionMetadata partitions = 3;
}

message PartitionMetadata {
  int32 error_code = 1;
  int32 partition_index = 2;
  int32 leader_id = 3;
}
```

---

### Produce

Write one or more messages to a topic partition.

**Request**
```protobuf
message ProduceRequest {
  int32 required_acks = 1;  // -1=all, 0=none, 1=leader
  int32 timeout_ms = 2;
  repeated TopicProduceData topics = 3;
}

message TopicProduceData {
  string name = 1;
  repeated PartitionProduceData partitions = 2;
}

message PartitionProduceData {
  int32 index = 1;
  repeated Message records = 2;
}

message Message {
  bytes key = 1;
  bytes value = 2;
}
```

**Response**
```protobuf
message ProduceResponse {
  repeated TopicProduceResponse topics = 1;
}

message TopicProduceResponse {
  string name = 1;
  repeated PartitionProduceResponse partitions = 2;
}

message PartitionProduceResponse {
  int32 index = 1;
  int32 error_code = 2;
  int64 base_offset = 3;  // Offset of the first message in the batch
}
```

---

### Fetch

Read messages from a topic partition starting at a given offset.

**Request**
```protobuf
message FetchRequest {
  int32 replica_id = 1;   // Set to -1 for consumer clients
  int32 max_wait_ms = 2;
  int32 min_bytes = 3;
  repeated FetchTopic topics = 4;
}

message FetchTopic {
  string topic = 1;
  repeated FetchPartition partitions = 2;
}

message FetchPartition {
  int32 partition = 1;
  int64 fetch_offset = 2;  // Start reading from this offset
  int32 max_bytes = 3;
}
```

**Response**
```protobuf
message FetchResponse {
  repeated FetchTopicResponse topics = 1;
}

message FetchTopicResponse {
  string topic = 1;
  repeated FetchPartitionResponse partitions = 2;
}

message FetchPartitionResponse {
  int32 partition = 1;
  int32 error_code = 2;
  int64 high_watermark = 3;  // Offset of the next message to be written
  repeated FetchedMessage records = 4;
}

message FetchedMessage {
  int64 offset = 1;
  bytes key = 2;
  bytes value = 3;
}
```

---

### ListOffsets

Query the earliest or latest available offset for a partition.

**Request**
```protobuf
message ListOffsetsRequest {
  int32 replica_id = 1;
  repeated ListOffsetsTopic topics = 2;
}

message ListOffsetsTopic {
  string name = 1;
  repeated ListOffsetsPartition partitions = 2;
}

message ListOffsetsPartition {
  int32 partition_index = 1;
  int64 timestamp = 2;  // -2=earliest, -1=latest
}
```

**Response**
```protobuf
message ListOffsetsResponse {
  repeated ListOffsetsTopicResponse topics = 1;
}

message ListOffsetsTopicResponse {
  string name = 1;
  repeated ListOffsetsPartitionResponse partitions = 2;
}

message ListOffsetsPartitionResponse {
  int32 partition_index = 1;
  int32 error_code = 2;
  int64 offset = 3;
}
```

---

### Consumer Group Operations

The following methods support consumer group management. See [Concepts](../concepts.md) for background.

#### FindCoordinator

Locate the broker responsible for a consumer group:

```protobuf
message FindCoordinatorRequest  { string key = 1; }
message FindCoordinatorResponse { int32 error_code = 1; string host = 2; int32 port = 3; }
```

#### JoinGroup / SyncGroup / Heartbeat / LeaveGroup

These methods implement the Kafka consumer group protocol for group membership and partition assignment. Refer to `src/api/kafka.proto` for full message definitions.

---

### CommitOffset

Persist the consumer's current position for a group/topic/partition:

**Request**
```protobuf
message OffsetCommitRequest {
  string group_id = 1;
  repeated OffsetCommitTopic topics = 2;
}

message OffsetCommitTopic {
  string name = 1;
  repeated OffsetCommitPartition partitions = 2;
}

message OffsetCommitPartition {
  int32 partition_index = 1;
  int64 committed_offset = 2;
}
```

### FetchOffset

Retrieve the last committed offset for a group/topic/partition:

**Request**
```protobuf
message OffsetFetchRequest {
  string group_id = 1;
  repeated OffsetFetchTopic topics = 2;
}
```

**Response** includes `committed_offset` per partition (`-1` if no offset has been committed).

---

## Using the API from Other Languages

Generate a client from the proto file:

```bash
# Go
protoc --go_out=. --go-grpc_out=. src/api/kafka.proto

# Python
python -m grpc_tools.protoc -I src/api --python_out=. --grpc_python_out=. kafka.proto

# Node.js (with @grpc/proto-loader)
# Load kafka.proto at runtime — no code generation needed
```

Refer to the [gRPC documentation](https://grpc.io/docs/) for language-specific setup.
