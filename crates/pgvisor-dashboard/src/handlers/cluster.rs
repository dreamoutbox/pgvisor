use std::sync::Arc;

use askama::Template;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Html;
use axum::Json;
use tokio::sync::RwLock;
use tracing::{error, info};

use super::state::DashboardState;
use crate::models::{
    ClusterOverview, NodeActionRequest, NodeActionResponse, NodeHealthState, NodeLifecycleAction,
    NodeRole, NodeSummary, SwitchoverRequest, SwitchoverResponse,
};
use crate::templates::{NodesTemplate, OverviewTemplate};

/// Abstraction for managing cluster lifecycle, node operations, and leader switchover.
#[async_trait::async_trait]
pub trait ClusterService: Send + Sync {
    async fn switchover(&self, target_node_id: u64) -> Result<SwitchoverResponse, String>;
    async fn start_node(&self, node_id: u64) -> Result<NodeActionResponse, String>;
    async fn stop_node(&self, node_id: u64) -> Result<NodeActionResponse, String>;
    async fn restart_node(&self, node_id: u64) -> Result<NodeActionResponse, String>;
}

/// In-memory cluster service for standalone testing or dashboard demo.
pub struct StandaloneClusterService {
    overview: Arc<RwLock<ClusterOverview>>,
}

impl StandaloneClusterService {
    pub fn new(overview: Arc<RwLock<ClusterOverview>>) -> Self {
        Self { overview }
    }
}

#[async_trait::async_trait]
impl ClusterService for StandaloneClusterService {
    async fn switchover(&self, target_node_id: u64) -> Result<SwitchoverResponse, String> {
        let mut ov = self.overview.write().await;
        let prev = ov.leader_id;
        for node in &mut ov.nodes {
            if node.node_id == target_node_id {
                node.role = NodeRole::Leader;
            } else if node.role == NodeRole::Leader {
                node.role = NodeRole::Standby;
            }
        }
        ov.leader_id = Some(target_node_id);

        let old_node = prev
            .map(|id| format!("node{}", id))
            .unwrap_or_else(|| "node1".to_string());
        let new_node = format!("node{}", target_node_id);
        pgvisor_core::log_highlight(&pgvisor_core::format_leader_down_highlight(
            &old_node, &new_node,
        ));

        Ok(SwitchoverResponse {
            status: "ok".into(),
            message: format!("Switched over leader to Node #{}", target_node_id),
            previous_leader_id: prev,
            new_leader_id: target_node_id,
        })
    }

    async fn start_node(&self, node_id: u64) -> Result<NodeActionResponse, String> {
        let mut ov = self.overview.write().await;
        if let Some(node) = ov.nodes.iter_mut().find(|n| n.node_id == node_id) {
            if node.state == NodeHealthState::Healthy {
                return Err(format!("Node #{} is already running", node_id));
            }
            node.state = NodeHealthState::Healthy;
            pgvisor_core::log_highlight("START NODE");
            Ok(NodeActionResponse {
                status: "ok".into(),
                message: format!("Node #{} started successfully", node_id),
                node_id,
                action: NodeLifecycleAction::Start,
            })
        } else {
            Err(format!("Node #{} not found", node_id))
        }
    }

    async fn stop_node(&self, node_id: u64) -> Result<NodeActionResponse, String> {
        let mut ov = self.overview.write().await;
        if let Some(node) = ov.nodes.iter_mut().find(|n| n.node_id == node_id) {
            if node.state == NodeHealthState::Stopped {
                return Err(format!("Node #{} is already stopped", node_id));
            }
            node.state = NodeHealthState::Stopped;
            pgvisor_core::log_highlight("STOP NODE");
            Ok(NodeActionResponse {
                status: "ok".into(),
                message: format!("Node #{} stopped cleanly", node_id),
                node_id,
                action: NodeLifecycleAction::Stop,
            })
        } else {
            Err(format!("Node #{} not found", node_id))
        }
    }

    async fn restart_node(&self, node_id: u64) -> Result<NodeActionResponse, String> {
        let mut ov = self.overview.write().await;
        if let Some(node) = ov.nodes.iter_mut().find(|n| n.node_id == node_id) {
            node.state = NodeHealthState::Healthy;
            node.uptime_secs = 0;
            Ok(NodeActionResponse {
                status: "ok".into(),
                message: format!("Node #{} restarted successfully", node_id),
                node_id,
                action: NodeLifecycleAction::Restart,
            })
        } else {
            Err(format!("Node #{} not found", node_id))
        }
    }
}

/// GET / -> Renders cluster overview dashboard
pub async fn get_overview(
    State(state): State<Arc<DashboardState>>,
) -> Result<Html<String>, StatusCode> {
    if let Ok(backups) = state.backup_service.list_backups().await {
        let mut overview_write = state.overview.write().await;
        overview_write.total_backups = backups.len();
        if let Some(latest) = backups.first() {
            overview_write.last_backup_at = Some(latest.created_at);
        }
    }

    let overview = state.overview.read().await;
    let template = OverviewTemplate {
        overview: &overview,
        auth_enabled: state.admin_token.is_some(),
    };
    template.render().map(Html).map_err(|e| {
        error!(?e, "Failed to render overview template");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

/// GET /nodes -> Renders detailed node health inspection
pub async fn get_nodes(
    State(state): State<Arc<DashboardState>>,
) -> Result<Html<String>, StatusCode> {
    let overview = state.overview.read().await;
    let template = NodesTemplate {
        nodes: &overview.nodes,
        auth_enabled: state.admin_token.is_some(),
    };
    template.render().map(Html).map_err(|e| {
        error!(?e, "Failed to render nodes template");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

/// GET /api/status -> Returns cluster health status JSON
pub async fn api_status(State(state): State<Arc<DashboardState>>) -> Json<ClusterOverview> {
    let overview = state.overview.read().await.clone();
    Json(overview)
}

/// GET /api/nodes -> Returns list of cluster nodes summary JSON
pub async fn api_nodes(State(state): State<Arc<DashboardState>>) -> Json<Vec<NodeSummary>> {
    let overview = state.overview.read().await;
    Json(overview.nodes.clone())
}

/// POST /api/cluster/switchover -> Initiates manual leader switchover to target node
pub async fn api_switchover(
    State(state): State<Arc<DashboardState>>,
    Json(payload): Json<SwitchoverRequest>,
) -> Result<Json<SwitchoverResponse>, (StatusCode, Json<serde_json::Value>)> {
    let target_node_id = payload.target_node_id;

    // Validate target node exists and is a healthy standby
    {
        let overview = state.overview.read().await;
        let target_node = overview.nodes.iter().find(|n| n.node_id == target_node_id);

        match target_node {
            None => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({
                        "error": format!("Target node #{} does not exist in cluster", target_node_id)
                    })),
                ));
            }
            Some(node) => {
                if node.role == NodeRole::Leader {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({
                            "error": format!("Node #{} is already the active leader", target_node_id)
                        })),
                    ));
                }
                if node.state != NodeHealthState::Healthy {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({
                            "error": format!("Target node #{} is not healthy (current state: {:?})", target_node_id, node.state)
                        })),
                    ));
                }
            }
        }
    }

    info!(target_node_id, "Initiating cluster leader switchover");

    let resp = state
        .cluster_service
        .switchover(target_node_id)
        .await
        .map_err(|e| {
            error!(target_node_id, ?e, "Switchover failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("Switchover failed: {}", e)
                })),
            )
        })?;

    // Update in-memory overview
    {
        let mut overview = state.overview.write().await;
        overview.leader_id = Some(resp.new_leader_id);
        for node in &mut overview.nodes {
            if node.node_id == resp.new_leader_id {
                node.role = NodeRole::Leader;
            } else if Some(node.node_id) == resp.previous_leader_id {
                node.role = NodeRole::Standby;
            }
        }
    }

    Ok(Json(resp))
}

async fn api_node_action_internal(
    state: Arc<DashboardState>,
    node_id: u64,
    action: NodeLifecycleAction,
) -> Result<Json<NodeActionResponse>, (StatusCode, Json<serde_json::Value>)> {
    // Validate target node exists in cluster overview
    {
        let overview = state.overview.read().await;
        if !overview.nodes.iter().any(|n| n.node_id == node_id) {
            return Err((
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": format!("Node #{} not found in cluster", node_id)
                })),
            ));
        }
    }

    info!(node_id, action = ?action, "Executing node lifecycle action from dashboard API");

    let res = match action {
        NodeLifecycleAction::Start => state.cluster_service.start_node(node_id).await,
        NodeLifecycleAction::Stop => state.cluster_service.stop_node(node_id).await,
        NodeLifecycleAction::Restart => state.cluster_service.restart_node(node_id).await,
    };

    match res {
        Ok(resp) => Ok(Json(resp)),
        Err(e) => {
            error!(node_id, action = ?action, ?e, "Node lifecycle action failed");
            let status = if e.contains("already") {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            Err((
                status,
                Json(serde_json::json!({
                    "error": e
                })),
            ))
        }
    }
}

/// POST /api/nodes/:node_id/start -> Starts PostgreSQL on target node
pub async fn api_start_node(
    State(state): State<Arc<DashboardState>>,
    Path(node_id): Path<u64>,
) -> Result<Json<NodeActionResponse>, (StatusCode, Json<serde_json::Value>)> {
    api_node_action_internal(state, node_id, NodeLifecycleAction::Start).await
}

/// POST /api/nodes/:node_id/stop -> Stops PostgreSQL on target node
pub async fn api_stop_node(
    State(state): State<Arc<DashboardState>>,
    Path(node_id): Path<u64>,
) -> Result<Json<NodeActionResponse>, (StatusCode, Json<serde_json::Value>)> {
    api_node_action_internal(state, node_id, NodeLifecycleAction::Stop).await
}

/// POST /api/nodes/:node_id/restart -> Restarts PostgreSQL on target node
pub async fn api_restart_node(
    State(state): State<Arc<DashboardState>>,
    Path(node_id): Path<u64>,
) -> Result<Json<NodeActionResponse>, (StatusCode, Json<serde_json::Value>)> {
    api_node_action_internal(state, node_id, NodeLifecycleAction::Restart).await
}

/// POST /api/nodes/:node_id/action -> Executes arbitrary lifecycle action on target node
pub async fn api_node_action(
    State(state): State<Arc<DashboardState>>,
    Path(node_id): Path<u64>,
    Json(payload): Json<NodeActionRequest>,
) -> Result<Json<NodeActionResponse>, (StatusCode, Json<serde_json::Value>)> {
    api_node_action_internal(state, node_id, payload.action).await
}
