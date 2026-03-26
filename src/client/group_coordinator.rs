use std::sync::Arc;

use tokio::time::{sleep, Duration};
use tonic::Request;

use crate::api::broker::{
    join_group_request, HeartbeatRequest, JoinGroupRequest, LeaveGroupRequest, SyncGroupRequest,
};
use crate::client::kafka_broker_client::{KafkaBrokerClient, KafkaBrokerClientTrait};

#[derive(Debug, Clone)]
pub struct GroupSession {
    pub group_id: String,
    pub member_id: String,
    pub generation_id: i32,
    pub assigned_partitions: Vec<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoordSignal {
    Healthy,
    RejoinRequired,
}

#[derive(thiserror::Error, Debug)]
pub enum CoordError {
    #[error("group coordinator not available: {0}")]
    Unavailable(String),
    #[error("join group failed: {0}")]
    JoinFailed(String),
    #[error("sync group failed: {0}")]
    SyncFailed(String),
    #[error("heartbeat failed: {0}")]
    HeartbeatFailed(String),
    #[error("leave group failed: {0}")]
    LeaveFailed(String),
}

#[async_trait::async_trait]
pub trait ConsumerGroupCoordinator: Send + Sync {
    async fn open(
        &self,
        group_id: &str,
        topic: &str,
        session_timeout_ms: i64,
        member_id_hint: Option<&str>,
    ) -> Result<GroupSession, CoordError>;
    async fn heartbeat(&self, session: &GroupSession) -> Result<CoordSignal, CoordError>;
    async fn leave(&self, session: &GroupSession) -> Result<(), CoordError>;
}

pub struct GrpcConsumerGroupCoordinator {
    client: Arc<KafkaBrokerClient>,
}

impl GrpcConsumerGroupCoordinator {
    pub async fn connect(addr: &str) -> Result<Self, CoordError> {
        let client = KafkaBrokerClient::new(addr)
            .await
            .map_err(|e| CoordError::Unavailable(e.to_string()))?;
        Ok(Self {
            client: Arc::new(client),
        })
    }

    pub fn new(client: Arc<KafkaBrokerClient>) -> Self {
        Self { client }
    }
}

#[async_trait::async_trait]
impl ConsumerGroupCoordinator for GrpcConsumerGroupCoordinator {
    async fn open(
        &self,
        group_id: &str,
        topic: &str,
        session_timeout_ms: i64,
        member_id_hint: Option<&str>,
    ) -> Result<GroupSession, CoordError> {
        const MAX_ATTEMPTS: u32 = 5;
        const REBALANCE_ERROR_CODE: i32 = 27;

        let mut member_id = member_id_hint
            .map(ToOwned::to_owned)
            .unwrap_or_else(uuid_simple);

        for attempt in 0..MAX_ATTEMPTS {
            if attempt > 0 {
                sleep(Duration::from_millis(300 * attempt as u64)).await;
            }

            let join_req = Request::new(JoinGroupRequest {
                group_id: group_id.to_string(),
                session_timeout: session_timeout_ms as i32,
                member_id: member_id.clone(),
                protocol_type: "consumer".to_string(),
                group_protocols: vec![join_group_request::GroupProtocol {
                    protocol_name: "range".to_string(),
                    protocol_metadata: topic.as_bytes().to_vec(),
                }],
            });

            let join = self
                .client
                .join_group(join_req)
                .await
                .map_err(|e| CoordError::JoinFailed(e.to_string()))?;

            if join.error_code == REBALANCE_ERROR_CODE {
                continue;
            }
            if join.error_code != 0 {
                return Err(CoordError::JoinFailed(format!(
                    "error_code={}",
                    join.error_code
                )));
            }

            member_id = join.member_id.clone();
            let sync_req = Request::new(SyncGroupRequest {
                group_id: group_id.to_string(),
                generation_id: join.generation_id,
                member_id: member_id.clone(),
                group_assignment: vec![],
            });
            let sync = self
                .client
                .sync_group(sync_req)
                .await
                .map_err(|e| CoordError::SyncFailed(e.to_string()))?;

            if sync.error_code == REBALANCE_ERROR_CODE {
                continue;
            }
            if sync.error_code != 0 {
                return Err(CoordError::SyncFailed(format!(
                    "error_code={}",
                    sync.error_code
                )));
            }

            let assigned_partitions: Vec<i32> = if sync.member_assignment.is_empty() {
                vec![]
            } else {
                bincode::deserialize(&sync.member_assignment).unwrap_or_default()
            };

            return Ok(GroupSession {
                group_id: group_id.to_string(),
                member_id,
                generation_id: join.generation_id,
                assigned_partitions,
            });
        }

        Err(CoordError::JoinFailed(format!(
            "persistent REBALANCE_IN_PROGRESS for group '{}'",
            group_id
        )))
    }

    async fn heartbeat(&self, session: &GroupSession) -> Result<CoordSignal, CoordError> {
        let req = Request::new(HeartbeatRequest {
            group_id: session.group_id.clone(),
            generation_id: session.generation_id,
            member_id: session.member_id.clone(),
        });
        let resp = self
            .client
            .heartbeat(req)
            .await
            .map_err(|e| CoordError::HeartbeatFailed(e.to_string()))?;
        match resp.error_code {
            0 => Ok(CoordSignal::Healthy),
            27 => Ok(CoordSignal::RejoinRequired),
            code => Err(CoordError::HeartbeatFailed(format!("error_code={code}"))),
        }
    }

    async fn leave(&self, session: &GroupSession) -> Result<(), CoordError> {
        let req = Request::new(LeaveGroupRequest {
            group_id: session.group_id.clone(),
            member_id: session.member_id.clone(),
        });
        let resp = self
            .client
            .leave_group(req)
            .await
            .map_err(|e| CoordError::LeaveFailed(e.to_string()))?;
        if resp.error_code == 0 {
            Ok(())
        } else {
            Err(CoordError::LeaveFailed(format!(
                "error_code={}",
                resp.error_code
            )))
        }
    }
}

fn uuid_simple() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let pid = std::process::id();
    format!("{}-{}-{}-{}", t.as_secs(), t.subsec_nanos(), pid, count)
}
