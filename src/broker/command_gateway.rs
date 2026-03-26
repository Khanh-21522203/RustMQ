use async_trait::async_trait;

use crate::broker::error::{BrokerError, BrokerResult};
use crate::broker::storage::{BrokerStorage, GroupMember};

pub enum BrokerCommand {
    CreateTopic {
        topic: String,
        num_partitions: i32,
    },
    Produce {
        topic: String,
        partition: i32,
        key: Option<Vec<u8>>,
        value: Vec<u8>,
    },
    CommitOffset {
        group_id: String,
        topic: String,
        partition: i32,
        offset: i64,
        metadata: String,
    },
    GroupJoin {
        group_id: String,
        member_id: String,
        topic: String,
        session_timeout_ms: i64,
    },
    GroupLeave {
        group_id: String,
        member_id: String,
    },
}

pub enum CommandOutcome {
    Ack,
    Offset(i64),
    GroupJoin {
        generation_id: i32,
        leader_id: String,
        member_id: String,
        members: Vec<GroupMember>,
    },
}

#[async_trait]
pub trait LeaderAwareCommandGateway: Send + Sync {
    async fn execute(&self, command: BrokerCommand) -> BrokerResult<CommandOutcome>;
}

pub struct StorageCommandGateway<S: BrokerStorage> {
    storage: S,
}

impl<S: BrokerStorage> StorageCommandGateway<S> {
    pub fn new(storage: S) -> Self {
        Self { storage }
    }
}

#[async_trait]
impl<S: BrokerStorage> LeaderAwareCommandGateway for StorageCommandGateway<S> {
    async fn execute(&self, command: BrokerCommand) -> BrokerResult<CommandOutcome> {
        match command {
            BrokerCommand::CreateTopic {
                topic,
                num_partitions,
            } => {
                self.storage.create_topic(&topic, num_partitions).await?;
                Ok(CommandOutcome::Ack)
            }
            BrokerCommand::Produce {
                topic,
                partition,
                key,
                value,
            } => {
                let offset = self
                    .storage
                    .produce_message(&topic, partition, key, value)
                    .await?;
                Ok(CommandOutcome::Offset(offset))
            }
            BrokerCommand::CommitOffset {
                group_id,
                topic,
                partition,
                offset,
                metadata,
            } => {
                self.storage
                    .commit_offset(&group_id, &topic, partition, offset, metadata)
                    .await?;
                Ok(CommandOutcome::Ack)
            }
            BrokerCommand::GroupJoin {
                group_id,
                member_id,
                topic,
                session_timeout_ms,
            } => {
                let (generation_id, leader_id, member_id, members) = self
                    .storage
                    .join_group(
                        &group_id,
                        &member_id,
                        "consumer",
                        topic.as_bytes(),
                        session_timeout_ms,
                    )
                    .await?;
                Ok(CommandOutcome::GroupJoin {
                    generation_id,
                    leader_id,
                    member_id,
                    members,
                })
            }
            BrokerCommand::GroupLeave {
                group_id,
                member_id,
            } => {
                self.storage.leave_group(&group_id, &member_id).await?;
                Ok(CommandOutcome::Ack)
            }
        }
    }
}

impl From<BrokerError> for tonic::Status {
    fn from(value: BrokerError) -> Self {
        tonic::Status::internal(value.to_string())
    }
}
