use serde::{Deserialize, Serialize};

/// Role of a node within the Raft + PostgreSQL cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeRole {
    Leader,
    Standby,
    Learner,
}

impl std::fmt::Display for NodeRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Leader => write!(f, "leader"),
            Self::Standby => write!(f, "standby"),
            Self::Learner => write!(f, "learner"),
        }
    }
}
