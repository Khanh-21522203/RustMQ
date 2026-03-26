use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrokerError {
    NotLeader { leader_addr: Option<String> },
    RebalanceInProgress,
    Validation(String),
    NotFound(String),
    Internal(String),
}

pub type BrokerResult<T> = Result<T, BrokerError>;

impl BrokerError {
    pub fn from_message(message: impl Into<String>) -> Self {
        let msg = message.into();
        if let Some(addr) = msg.strip_prefix("NOT_LEADER:") {
            let leader_addr = if addr.is_empty() {
                None
            } else {
                Some(addr.to_string())
            };
            return Self::NotLeader { leader_addr };
        }
        if msg == "REBALANCE_IN_PROGRESS" {
            return Self::RebalanceInProgress;
        }
        Self::Internal(msg)
    }

    pub fn leader_addr(&self) -> Option<&str> {
        match self {
            Self::NotLeader { leader_addr } => leader_addr.as_deref(),
            _ => None,
        }
    }
}

impl fmt::Display for BrokerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotLeader {
                leader_addr: Some(addr),
            } => write!(f, "NOT_LEADER:{addr}"),
            Self::NotLeader { leader_addr: None } => write!(f, "NOT_LEADER:"),
            Self::RebalanceInProgress => write!(f, "REBALANCE_IN_PROGRESS"),
            Self::Validation(msg) => write!(f, "{msg}"),
            Self::NotFound(msg) => write!(f, "{msg}"),
            Self::Internal(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for BrokerError {}

impl From<anyhow::Error> for BrokerError {
    fn from(value: anyhow::Error) -> Self {
        Self::from_message(value.to_string())
    }
}
