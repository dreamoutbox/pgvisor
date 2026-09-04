use openraft::declare_raft_types;
use serde::{Deserialize, Serialize};

/// High-level cluster node operational role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeRole {
    Leader,
    Standby,
    Fenced,
}

/// Metadata describing a cluster node endpoint and Postgres address.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NodeInfo {
    pub rpc_addr: String,
    pub pg_addr: String,
}

/// State machine write commands replicated via OpenRaft consensus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClusterCommand {
    RegisterNode { node_id: u64, info: NodeInfo },
    UpdateNodeRole { node_id: u64, role: NodeRole },
    SetState { key: String, value: String },
    DeleteState { key: String },
}

/// Response returned after committing and applying a `ClusterCommand`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClusterResponse {
    Success,
    Value(Option<String>),
    Error(String),
}

// Declare the concrete OpenRaft type configuration for PgVisor.
declare_raft_types!(
    pub TypeConfig:
        D = ClusterCommand,
        R = ClusterResponse,
        NodeId = u64,
        Node = NodeInfo,
        SnapshotData = std::io::Cursor<Vec<u8>>,
);
