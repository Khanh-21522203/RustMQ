use tokio::sync::mpsc;
use std::sync::Arc;
use tokio::sync::Mutex;
use crate::api::requests::BrokerGrpcRequest;
use crate::api::responses::BrokerGrpcResponse;
use crate::api::broker::*;
use crate::broker::storage::BrokerStorage;

pub struct BrokerCore<S: BrokerStorage> {
    rpc_receive_channel: mpsc::Receiver<(BrokerGrpcRequest, mpsc::Sender<BrokerGrpcResponse>)>,
    storage: Arc<Mutex<S>>,
}

impl<S: BrokerStorage + 'static> BrokerCore<S> {
    pub fn new(
        rpc_receive_channel: mpsc::Receiver<(BrokerGrpcRequest, mpsc::Sender<BrokerGrpcResponse>)>,
        storage: S,
    ) -> Self {
        Self {
            rpc_receive_channel,
            storage: Arc::new(Mutex::new(storage)),
        }
    }
    
    pub async fn run(&mut self) {
        loop {
            let event = self.rpc_receive_channel.recv().await;
            self.handle_rpc_event(event).await;
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
    async fn handle_get_topic_metadata(&mut self, request: TopicMetadataRequest) -> TopicMetadataResponse {
        let storage = self.storage.lock().await;
        
        let topics = if request.topics.is_empty() {
            storage.get_topics().await
        } else {
            request.topics
        };

        let mut topic_metadata = Vec::new();
        for topic_name in topics {
            if let Some(partitions) = storage.get_topic_partitions(&topic_name).await {
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

        let (broker_id, host, port) = storage.get_coordinator_info().await;
        TopicMetadataResponse {
            brokers: vec![topic_metadata_response::Broker {
                node_id: broker_id,
                host,
                port,
            }],
            topics: topic_metadata,
        }
    }

    async fn handle_produce(&mut self, request: ProduceRequest) -> ProduceResponse {
        let mut storage = self.storage.lock().await;
        let mut results = Vec::new();

        for topic_data in request.topics {
            let mut partition_results = Vec::new();
            
            for partition_data in topic_data.partitions {
                match storage.produce_message(
                    &topic_data.topic_name,
                    partition_data.partition,
                    partition_data.message_set,
                ).await {
                    Ok(offset) => {
                        partition_results.push(produce_response::PartitionResult {
                            partition: partition_data.partition,
                            error_code: 0,
                            offset,
                        });
                    }
                    Err(e) => {
                        log::error!("Failed to produce message: {}", e);
                        partition_results.push(produce_response::PartitionResult {
                            partition: partition_data.partition,
                            error_code: 1,
                            offset: -1,
                        });
                    }
                }
            }

            results.push(produce_response::TopicResult {
                topic_name: topic_data.topic_name,
                partitions: partition_results,
            });
        }

        ProduceResponse { results }
    }

    async fn handle_fetch(&mut self, request: FetchRequest) -> FetchResponse {
        let storage = self.storage.lock().await;
        let mut topics = Vec::new();

        for topic_data in request.topics {
            let mut partition_results = Vec::new();

            for partition_data in topic_data.partitions {
                match storage.fetch_messages(
                    &topic_data.topic_name,
                    partition_data.partition,
                    partition_data.fetch_offset,
                    partition_data.max_bytes,
                ).await {
                    Ok((message_set, high_watermark)) => {
                        let message_set_size = message_set.len() as i32;
                        partition_results.push(fetch_response::PartitionResult {
                            partition: partition_data.partition,
                            error_code: 0,
                            high_watermark_offset: high_watermark,
                            message_set_size,
                            message_set,
                        });
                    }
                    Err(e) => {
                        log::error!("Failed to fetch messages: {}", e);
                        partition_results.push(fetch_response::PartitionResult {
                            partition: partition_data.partition,
                            error_code: 1,
                            high_watermark_offset: 0,
                            message_set_size: 0,
                            message_set: Vec::new(),
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

    async fn handle_list_offsets(&mut self, request: ListOffsetsRequest) -> ListOffsetsResponse {
        let storage = self.storage.lock().await;
        let mut topics = Vec::new();

        for topic_data in request.topics {
            let mut partition_offsets = Vec::new();

            for partition_data in topic_data.partitions {
                match storage.get_partition_offset(
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

    async fn handle_find_coordinator(&mut self, request: GroupCoordinatorRequest) -> GroupCoordinatorResponse {
        let storage = self.storage.lock().await;
        let (coordinator_id, coordinator_host, coordinator_port) = 
            storage.get_coordinator_info().await;

        GroupCoordinatorResponse {
            error_code: 0,
            coordinator_id,
            coordinator_host,
            coordinator_port,
        }
    }

    async fn handle_join_group(&mut self, request: JoinGroupRequest) -> JoinGroupResponse {
        let mut storage = self.storage.lock().await;
        
        match storage.join_group(
            &request.group_id,
            &request.member_id,
            &request.protocol_type,
        ).await {
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

    async fn handle_sync_group(&mut self, request: SyncGroupRequest) -> SyncGroupResponse {
        let mut storage = self.storage.lock().await;
        
        match storage.sync_group(
            &request.group_id,
            request.generation_id,
            &request.member_id,
        ).await {
            Ok(member_assignment) => {
                SyncGroupResponse {
                    error_code: 0,
                    member_assignment,
                }
            }
            Err(e) => {
                log::error!("Failed to sync group: {}", e);
                SyncGroupResponse {
                    error_code: 1,
                    member_assignment: Vec::new(),
                }
            }
        }
    }

    async fn handle_heartbeat(&mut self, request: HeartbeatRequest) -> HeartbeatResponse {
        let mut storage = self.storage.lock().await;
        
        match storage.heartbeat(
            &request.group_id,
            request.generation_id,
            &request.member_id,
        ).await {
            Ok(_) => HeartbeatResponse { error_code: 0 },
            Err(e) => {
                log::error!("Heartbeat failed: {}", e);
                HeartbeatResponse { error_code: 1 }
            }
        }
    }

    async fn handle_leave_group(&mut self, request: LeaveGroupRequest) -> LeaveGroupResponse {
        let mut storage = self.storage.lock().await;
        
        match storage.leave_group(&request.group_id, &request.member_id).await {
            Ok(_) => LeaveGroupResponse { error_code: 0 },
            Err(e) => {
                log::error!("Failed to leave group: {}", e);
                LeaveGroupResponse { error_code: 1 }
            }
        }
    }

    async fn handle_commit_offset(&mut self, request: OffsetCommitRequest) -> OffsetCommitResponse {
        let mut storage = self.storage.lock().await;
        let mut topics = Vec::new();

        for topic_data in request.topics {
            let mut partition_results = Vec::new();

            for partition_data in topic_data.partitions {
                match storage.commit_offset(
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

    async fn handle_fetch_offset(&mut self, request: OffsetFetchRequest) -> OffsetFetchResponse {
        let storage = self.storage.lock().await;
        let mut topics = Vec::new();

        for topic_data in request.topics {
            let mut partition_results = Vec::new();

            for partition in topic_data.partitions {
                match storage.fetch_offset(
                    &request.consumer_group,
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
