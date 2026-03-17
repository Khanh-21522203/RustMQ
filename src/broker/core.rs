use tokio::sync::{mpsc, oneshot};
use crate::api::requests::BrokerGrpcRequest;
use crate::api::responses::BrokerGrpcResponse;
use crate::api::broker::*;
use crate::broker::storage::BrokerStorage;

pub struct BrokerCore<S: BrokerStorage> {
    rpc_receive_channel: mpsc::Receiver<(BrokerGrpcRequest, oneshot::Sender<BrokerGrpcResponse>)>,
    storage: S,
}

impl<S: BrokerStorage + 'static> BrokerCore<S> {
    pub fn new(
        rpc_receive_channel: mpsc::Receiver<(BrokerGrpcRequest, oneshot::Sender<BrokerGrpcResponse>)>,
        storage: S,
    ) -> Self {
        Self { rpc_receive_channel, storage }
    }

    pub async fn run(mut self) {
        loop {
            match self.rpc_receive_channel.recv().await {
                Some((request, response_sender)) => {
                    self.handle_rpc(request, response_sender).await;
                }
                None => {
                    log::info!("RPC channel closed; broker core shutting down");
                    break;
                }
            }
        }
    }

    async fn handle_rpc(
        &mut self,
        request: BrokerGrpcRequest,
        response_sender: oneshot::Sender<BrokerGrpcResponse>,
    ) {
        let response = match request {
            BrokerGrpcRequest::GetTopicMetadata(req) => {
                BrokerGrpcResponse::GetTopicMetadata(self.handle_get_topic_metadata(req).await)
            }
            BrokerGrpcRequest::Produce(req) => {
                BrokerGrpcResponse::Produce(self.handle_produce(req).await)
            }
            BrokerGrpcRequest::Fetch(req) => {
                BrokerGrpcResponse::Fetch(self.handle_fetch(req).await)
            }
            BrokerGrpcRequest::ListOffsets(req) => {
                BrokerGrpcResponse::ListOffsets(self.handle_list_offsets(req).await)
            }
            BrokerGrpcRequest::FindCoordinator(req) => {
                BrokerGrpcResponse::FindCoordinator(self.handle_find_coordinator(req).await)
            }
            BrokerGrpcRequest::JoinGroup(req) => {
                BrokerGrpcResponse::JoinGroup(self.handle_join_group(req).await)
            }
            BrokerGrpcRequest::SyncGroup(req) => {
                BrokerGrpcResponse::SyncGroup(self.handle_sync_group(req).await)
            }
            BrokerGrpcRequest::Heartbeat(req) => {
                BrokerGrpcResponse::Heartbeat(self.handle_heartbeat(req).await)
            }
            BrokerGrpcRequest::LeaveGroup(req) => {
                BrokerGrpcResponse::LeaveGroup(self.handle_leave_group(req).await)
            }
            BrokerGrpcRequest::CommitOffset(req) => {
                BrokerGrpcResponse::CommitOffset(self.handle_commit_offset(req).await)
            }
            BrokerGrpcRequest::FetchOffset(req) => {
                BrokerGrpcResponse::FetchOffset(self.handle_fetch_offset(req).await)
            }
            BrokerGrpcRequest::CreateTopic(req) => {
                BrokerGrpcResponse::CreateTopic(self.handle_create_topic(req).await)
            }
        };
        let _ = response_sender.send(response);
    }

    async fn handle_get_topic_metadata(&self, request: TopicMetadataRequest) -> TopicMetadataResponse {
        let topics = if request.topics.is_empty() {
            self.storage.get_topics().await
        } else {
            request.topics
        };

        let mut topic_metadata = Vec::new();
        for topic_name in topics {
            if let Some(partitions) = self.storage.get_topic_partitions(&topic_name).await {
                let partition_metadata: Vec<topic_metadata_response::PartitionMetadata> = partitions
                    .iter()
                    .map(|&partition_id| topic_metadata_response::PartitionMetadata {
                        partition_error_code: 0,
                        partition_id,
                        leader: 1,
                        replicas: vec![1],
                        isr: vec![1],
                    })
                    .collect();
                topic_metadata.push(topic_metadata_response::TopicMetadata {
                    topic_error_code: 0,
                    topic_name,
                    partitions: partition_metadata,
                });
            }
        }

        let (broker_id, host, port) = self.storage.get_coordinator_info().await;
        TopicMetadataResponse {
            brokers: vec![topic_metadata_response::Broker { node_id: broker_id, host, port }],
            topics: topic_metadata,
        }
    }

    async fn handle_produce(&self, request: ProduceRequest) -> ProduceResponse {
        let mut results = Vec::new();
        for topic_data in request.topics {
            let mut partition_results = Vec::new();
            for partition_data in topic_data.partitions {
                // Produce each record in the partition batch
                let mut last_offset = -1i64;
                let mut error_code = 0i32;
                let mut leader_addr = String::new();
                for record in partition_data.records {
                    match self.storage.produce_message(
                        &topic_data.topic_name,
                        partition_data.partition,
                        record.key,
                        record.value,
                    ).await {
                        Ok(offset) => { last_offset = offset; }
                        Err(e) => {
                            let err_str = e.to_string();
                            if let Some(addr) = err_str.strip_prefix("NOT_LEADER:") {
                                error_code = 6; // NOT_LEADER_FOR_PARTITION
                                leader_addr = addr.to_string();
                            } else {
                                log::error!("Failed to produce message: {}", err_str);
                                error_code = 1;
                            }
                        }
                    }
                }
                partition_results.push(produce_response::PartitionResult {
                    partition: partition_data.partition,
                    error_code,
                    offset: last_offset,
                    leader_addr,
                });
            }
            results.push(produce_response::TopicResult {
                topic_name: topic_data.topic_name,
                partitions: partition_results,
            });
        }
        ProduceResponse { results }
    }

    async fn handle_fetch(&self, request: FetchRequest) -> FetchResponse {
        let mut topics = Vec::new();
        for topic_data in request.topics {
            let mut partition_results = Vec::new();
            for partition_data in topic_data.partitions {
                match self.storage.fetch_messages(
                    &topic_data.topic_name,
                    partition_data.partition,
                    partition_data.fetch_offset,
                    partition_data.max_bytes,
                ).await {
                    Ok(messages) => {
                        let high_watermark = messages.last().map_or(partition_data.fetch_offset, |m| m.offset + 1);
                        let records: Vec<FetchedRecord> = messages
                            .into_iter()
                            .map(|m| FetchedRecord {
                                offset: m.offset,
                                key: m.key,
                                value: m.value,
                            })
                            .collect();
                        partition_results.push(fetch_response::PartitionResult {
                            partition: partition_data.partition,
                            error_code: 0,
                            high_watermark_offset: high_watermark,
                            records,
                        });
                    }
                    Err(e) => {
                        log::error!("Failed to fetch messages: {}", e);
                        partition_results.push(fetch_response::PartitionResult {
                            partition: partition_data.partition,
                            error_code: 1,
                            high_watermark_offset: 0,
                            records: vec![],
                        });
                    }
                }
            }
            topics.push(fetch_response::TopicResult {
                topic_name: topic_data.topic_name,
                partitions: partition_results,
            });
        }
        FetchResponse { topics }
    }

    async fn handle_list_offsets(&self, request: ListOffsetsRequest) -> ListOffsetsResponse {
        let mut topics = Vec::new();
        for topic_data in request.topics {
            let mut partition_offsets = Vec::new();
            for partition_data in topic_data.partitions {
                match self.storage.get_partition_offset(
                    &topic_data.topic_name,
                    partition_data.partition,
                    partition_data.time,
                ).await {
                    Ok(offsets) => {
                        partition_offsets.push(list_offsets_response::PartitionOffsets {
                            partition: partition_data.partition,
                            error_code: 0,
                            offsets,
                        });
                    }
                    Err(e) => {
                        log::error!("Failed to list offsets: {}", e);
                        partition_offsets.push(list_offsets_response::PartitionOffsets {
                            partition: partition_data.partition,
                            error_code: 1,
                            offsets: Vec::new(),
                        });
                    }
                }
            }
            topics.push(list_offsets_response::TopicResult {
                topic_name: topic_data.topic_name,
                partitions: partition_offsets,
            });
        }
        ListOffsetsResponse { topics }
    }

    async fn handle_find_coordinator(&self, _request: GroupCoordinatorRequest) -> GroupCoordinatorResponse {
        let (coordinator_id, coordinator_host, coordinator_port) =
            self.storage.get_coordinator_info().await;
        GroupCoordinatorResponse {
            error_code: 0,
            coordinator_id,
            coordinator_host,
            coordinator_port,
        }
    }

    async fn handle_join_group(&self, request: JoinGroupRequest) -> JoinGroupResponse {
        let protocol_metadata = request.group_protocols.first()
            .map(|p| p.protocol_metadata.clone())
            .unwrap_or_default();
        match self.storage.join_group(&request.group_id, &request.member_id, &request.protocol_type, &protocol_metadata, request.session_timeout as i64).await {
            Ok((generation_id, leader_id, member_id, members)) => {
                let protocol = request.group_protocols.first()
                    .map(|p| p.protocol_name.clone())
                    .unwrap_or_default();
                let member_list: Vec<join_group_response::Member> = members
                    .iter()
                    .map(|m| join_group_response::Member {
                        member_id: m.member_id.clone(),
                        member_metadata: m.metadata.clone(),
                    })
                    .collect();
                JoinGroupResponse {
                    error_code: 0,
                    generation_id,
                    group_protocol: protocol,
                    leader_id,
                    member_id,
                    members: member_list,
                }
            }
            Err(e) => {
                log::error!("Failed to join group: {}", e);
                JoinGroupResponse {
                    error_code: 1,
                    generation_id: -1,
                    group_protocol: String::new(),
                    leader_id: String::new(),
                    member_id: String::new(),
                    members: Vec::new(),
                }
            }
        }
    }

    async fn handle_sync_group(&self, request: SyncGroupRequest) -> SyncGroupResponse {
        match self.storage.sync_group(&request.group_id, request.generation_id, &request.member_id).await {
            Ok(member_assignment) => SyncGroupResponse { error_code: 0, member_assignment },
            Err(e) if e == "REBALANCE_IN_PROGRESS" => {
                SyncGroupResponse { error_code: 27, member_assignment: Vec::new() }
            }
            Err(e) => {
                log::error!("Failed to sync group: {}", e);
                SyncGroupResponse { error_code: 1, member_assignment: Vec::new() }
            }
        }
    }

    async fn handle_heartbeat(&self, request: HeartbeatRequest) -> HeartbeatResponse {
        match self.storage.heartbeat(&request.group_id, request.generation_id, &request.member_id).await {
            Ok(_) => HeartbeatResponse { error_code: 0 },
            Err(e) if e == "REBALANCE_IN_PROGRESS" => HeartbeatResponse { error_code: 27 },
            Err(e) => {
                log::error!("Heartbeat failed: {}", e);
                HeartbeatResponse { error_code: 1 }
            }
        }
    }

    async fn handle_leave_group(&self, request: LeaveGroupRequest) -> LeaveGroupResponse {
        match self.storage.leave_group(&request.group_id, &request.member_id).await {
            Ok(_) => LeaveGroupResponse { error_code: 0 },
            Err(e) => {
                log::error!("Failed to leave group: {}", e);
                LeaveGroupResponse { error_code: 1 }
            }
        }
    }

    async fn handle_commit_offset(&self, request: OffsetCommitRequest) -> OffsetCommitResponse {
        let mut topics = Vec::new();
        for topic_data in request.topics {
            let mut partition_results = Vec::new();
            for partition_data in topic_data.partitions {
                match self.storage.commit_offset(
                    &request.consumer_group_id,
                    &topic_data.topic_name,
                    partition_data.partition,
                    partition_data.offset,
                    partition_data.metadata,
                ).await {
                    Ok(_) => {
                        partition_results.push(offset_commit_response::PartitionResult {
                            partition: partition_data.partition,
                            error_code: 0,
                        });
                    }
                    Err(e) => {
                        log::error!("Failed to commit offset: {}", e);
                        partition_results.push(offset_commit_response::PartitionResult {
                            partition: partition_data.partition,
                            error_code: 1,
                        });
                    }
                }
            }
            topics.push(offset_commit_response::TopicResult {
                topic_name: topic_data.topic_name,
                partitions: partition_results,
            });
        }
        OffsetCommitResponse { topics }
    }

    async fn handle_create_topic(&self, request: CreateTopicRequest) -> CreateTopicResponse {
        match self.storage.create_topic(&request.topic_name, request.num_partitions).await {
            Ok(_) => CreateTopicResponse { error_code: 0, error_message: String::new() },
            Err(e) => {
                log::error!("Failed to create topic: {}", e);
                CreateTopicResponse { error_code: 1, error_message: e }
            }
        }
    }

    async fn handle_fetch_offset(&self, request: OffsetFetchRequest) -> OffsetFetchResponse {
        let mut topics = Vec::new();
        for topic_data in request.topics {
            let mut partition_results = Vec::new();
            for partition in topic_data.partitions {
                match self.storage.fetch_offset(
                    &request.consumer_group_id,
                    &topic_data.topic_name,
                    partition,
                ).await {
                    Ok((offset, metadata)) => {
                        partition_results.push(offset_fetch_response::PartitionResult {
                            partition,
                            offset,
                            metadata,
                            error_code: 0,
                        });
                    }
                    Err(e) => {
                        log::error!("Failed to fetch offset: {}", e);
                        partition_results.push(offset_fetch_response::PartitionResult {
                            partition,
                            offset: -1,
                            metadata: String::new(),
                            error_code: 1,
                        });
                    }
                }
            }
            topics.push(offset_fetch_response::TopicResult {
                topic_name: topic_data.topic_name,
                partitions: partition_results,
            });
        }
        OffsetFetchResponse { topics }
    }
}
