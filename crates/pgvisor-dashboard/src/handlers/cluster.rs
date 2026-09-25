use std::sync::Arc;

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Html;
use axum::Json;
use serde::Deserialize;
use tokio::sync::RwLock;
use tracing::{error, info};

use super::state::DashboardState;
use crate::models::{
    ClusterOverview, LogLevel, NodeActionRequest, NodeActionResponse, NodeConfigResponse,
    NodeConfigType, NodeHealthState, NodeLifecycleAction, NodeLogEntry, NodeLogsResponse,
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
    async fn get_node_logs(&self, node_id: u64, limit: usize) -> Result<NodeLogsResponse, String>;
    async fn get_node_config(&self, node_id: u64, config_type: NodeConfigType) -> Result<NodeConfigResponse, String>;
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

    async fn get_node_logs(&self, node_id: u64, limit: usize) -> Result<NodeLogsResponse, String> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let mock_entries = vec![
            NodeLogEntry {
                timestamp_ms: now.saturating_sub(60_000),
                level: LogLevel::Info,
                message: format!("PostgreSQL 18.6 started for node #{}", node_id),
            },
            NodeLogEntry {
                timestamp_ms: now.saturating_sub(45_000),
                level: LogLevel::Info,
                message: "database system was shut down at 2026-09-25 10:00:00 UTC".to_string(),
            },
            NodeLogEntry {
                timestamp_ms: now.saturating_sub(30_000),
                level: LogLevel::Info,
                message: "database system is ready to accept connections".to_string(),
            },
            NodeLogEntry {
                timestamp_ms: now.saturating_sub(10_000),
                level: LogLevel::Info,
                message: format!("checkpoint complete for node #{}: wrote 32 buffers (0.2%)", node_id),
            },
        ];
        let entries = mock_entries.into_iter().take(limit).collect();
        Ok(NodeLogsResponse {
            node_id,
            total_buffered: 4,
            entries,
        })
    }

    async fn get_node_config(&self, node_id: u64, config_type: NodeConfigType) -> Result<NodeConfigResponse, String> {
        let content = match config_type {
            NodeConfigType::PostgresqlConf => format!(
                "# postgresql.conf for node #{node_id}\nlisten_addresses = '*'\nport = 5432\nmax_connections = 100\nshared_buffers = 128MB\nwal_level = replica\nmax_wal_senders = 10\n"
            ),
            NodeConfigType::PostgresqlAutoConf => format!(
                "# Do not edit this file manually!\n# It will be overwritten by the ALTER SYSTEM command.\nprimary_conninfo = 'host=node1 port=5432 user=replicator'\n"
            ),
            NodeConfigType::PgHbaConf => format!(
                "# pg_hba.conf for node #{node_id}\nlocal all all trust\nhost all all 127.0.0.1/32 trust\nhost all all ::1/128 trust\nhost all all all md5\nhost replication replicator all md5\n"
            ),
            NodeConfigType::PgIdentConf => "# pg_ident.conf\n# MAPNAME SYSTEM-USERNAME PG-USERNAME\n".to_string(),
            NodeConfigType::PostmasterPid => "1234\n/var/lib/postgresql/data\n1727260800\n5432\n/tmp\n*\n123456\nready\n".to_string(),
            NodeConfigType::PostmasterOpts => "/usr/lib/postgresql/18/bin/postgres -D /var/lib/postgresql/data\n".to_string(),
            NodeConfigType::StandbySignal => String::new(),
            NodeConfigType::RecoverySignal => String::new(),
            NodeConfigType::BackupLabel => "START WAL LOCATION: 0/16000028 (file 000000010000000000000016)\nCHECKPOINT LOCATION: 0/16000060\nBACKUP METHOD: pg_basebackup\nBACKUP FROM: primary\nSTART TIME: 2026-09-25 10:15:00 UTC\n".to_string(),
        };

        let exists = match config_type {
            NodeConfigType::StandbySignal | NodeConfigType::RecoverySignal => false,
            _ => true,
        };

        Ok(NodeConfigResponse {
            node_id,
            file_type: config_type,
            filename: config_type.filename().to_string(),
            exists,
            size_bytes: content.len() as u64,
            modified_at_ms: Some(1727260800000),
            content,
        })
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

#[derive(Debug, Deserialize)]
pub struct LogsQuery {
    pub limit: Option<usize>,
}

/// GET /api/nodes/:node_id/logs?limit=N -> Returns recent log entries from node
pub async fn api_node_logs(
    State(state): State<Arc<DashboardState>>,
    Path(node_id): Path<u64>,
    Query(query): Query<LogsQuery>,
) -> Result<Json<NodeLogsResponse>, (StatusCode, Json<serde_json::Value>)> {
    let limit = query.limit.unwrap_or(100).min(1000).max(1);
    match state.cluster_service.get_node_logs(node_id, limit).await {
        Ok(resp) => Ok(Json(resp)),
        Err(e) => {
            error!(node_id, ?e, "Failed to retrieve node logs");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e })),
            ))
        }
    }
}

/// GET /api/nodes/:node_id/config/:config_type -> Returns inspected config/diagnostic file
pub async fn api_node_config(
    State(state): State<Arc<DashboardState>>,
    Path((node_id, file_type_slug)): Path<(u64, String)>,
) -> Result<Json<NodeConfigResponse>, (StatusCode, Json<serde_json::Value>)> {
    let config_type = match NodeConfigType::from_slug(&file_type_slug) {
        Some(ct) => ct,
        None => {
            let supported: Vec<&str> = NodeConfigType::all().iter().map(|c| c.to_slug()).collect();
            return Err((
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!("Invalid config file slug '{}'. Supported slugs: {:?}", file_type_slug, supported)
                })),
            ));
        }
    };

    match state.cluster_service.get_node_config(node_id, config_type).await {
        Ok(resp) => Ok(Json(resp)),
        Err(e) => {
            error!(node_id, ?e, "Failed to retrieve node configuration");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e })),
            ))
        }
    }
}
