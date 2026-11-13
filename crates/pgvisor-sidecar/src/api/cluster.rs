use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::Json;
use pgvisor_core::api::{
    ClusterOverview, NodeConfigResponse, NodeConfigType, NodeHealthState, NodeLogsResponse,
    NodeRole, NodeSummary, SidecarRole,
};
use pgvisor_core::auth::make_auth_header_value;
use serde::Deserialize;

use super::api_state::ApiState;
use super::error::ApiError;
use super::metrics::{lookup_lag_for_peer, query_replication_lags};
use super::peer_probe::{probe_all_peers, PeerProbe};

const DEFAULT_LOG_LIMIT: usize = 100;
const MAX_LOG_LIMIT: usize = 1000;

/// Query parameters accepted by `GET /api/nodes/:node_id/logs`.
#[derive(Debug, Default, Deserialize)]
pub struct LogsQuery {
    pub limit: Option<usize>,
}

fn map_peer_to_summary(
    probe: &PeerProbe,
    replica_lags: &HashMap<String, u64>,
) -> Option<NodeSummary> {
    let status = probe.status.as_ref()?;
    let role_enum = SidecarRole::from_role_str(&status.role);
    let role = match role_enum {
        SidecarRole::Leader => NodeRole::Leader,
        SidecarRole::Standby | SidecarRole::Fenced => NodeRole::Standby,
    };

    let health_state = match status.status.as_str() {
        "running" => NodeHealthState::Healthy,
        "restoring" => NodeHealthState::Degraded,
        "fenced" => NodeHealthState::Fenced,
        "stopped" => NodeHealthState::Stopped,
        _ => NodeHealthState::Offline,
    };

    let replication_lag_bytes = if role == NodeRole::Leader {
        0
    } else {
        lookup_lag_for_peer(replica_lags, status.node_id, &probe.pg_address)
    };

    Some(NodeSummary {
        node_id: status.node_id,
        address: probe.pg_address.clone(),
        role,
        state: health_state,
        pg_version: status.pg_version.clone(),
        replication_lag_bytes,
        uptime_secs: status.uptime_secs,
        is_local: probe.is_self,
        cpu_percent: status.cpu_percent,
        memory_used_bytes: status.memory_used_bytes,
        memory_total_bytes: status.memory_total_bytes,
        disk_used_bytes: status.disk_used_bytes,
    })
}

/// Builds a [`ClusterOverview`] from peer `/control/status` responses and local S3 backup metadata.
pub async fn collect_cluster_overview(state: &ApiState) -> ClusterOverview {
    let probes = probe_all_peers(state).await;

    let replica_lags = if state.sidecar.current_role().await == SidecarRole::Leader {
        query_replication_lags(&state.pg_addr, &state.pg_superuser).await
    } else {
        HashMap::new()
    };

    let mut nodes = Vec::with_capacity(probes.len());
    let mut healthy_nodes = 0usize;
    let mut leader_id = None;
    let mut leader_address = None;

    for probe in &probes {
        if let Some(summary) = map_peer_to_summary(probe, &replica_lags) {
            if summary.state == NodeHealthState::Healthy {
                healthy_nodes += 1;
            }
            if summary.role == NodeRole::Leader && summary.state == NodeHealthState::Healthy {
                leader_id = Some(summary.node_id);
                leader_address = Some(summary.address.clone());
            }
            nodes.push(summary);
        } else if let Some(node_id) = probe.node_id {
            nodes.push(NodeSummary {
                node_id,
                address: probe.pg_address.clone(),
                role: NodeRole::Standby,
                state: NodeHealthState::Offline,
                pg_version: state.sidecar.pg_version.clone(),
                replication_lag_bytes: 0,
                uptime_secs: 0,
                is_local: false,
                cpu_percent: 0.0,
                memory_used_bytes: 0,
                memory_total_bytes: 0,
                disk_used_bytes: 0,
            });
        }
    }

    let (total_backups, last_backup_at) = match state.sidecar.backup_manager {
        Some(ref manager) => match manager.list_basebackups().await {
            Ok(list) => {
                let latest = list.iter().map(|b| b.created_at).max();
                (list.len(), latest)
            }
            Err(_) => (0, None),
        },
        None => (0, None),
    };

    let total_nodes = nodes.len();
    let quorum_size = (total_nodes / 2) + 1;

    ClusterOverview {
        cluster_id: state.cluster_id.clone(),
        current_term: if leader_id.is_some() { 1 } else { 0 },
        leader_id,
        leader_address,
        quorum_size,
        total_nodes,
        healthy_nodes,
        last_backup_at,
        total_backups,
        nodes,
    }
}

/// Finds the peer entry matching `node_id`, failing with 404 if unknown.
pub(crate) async fn find_peer_by_node_id(
    state: &ApiState,
    node_id: u64,
) -> Result<PeerProbe, ApiError> {
    let probes = probe_all_peers(state).await;
    probes
        .into_iter()
        .find(|p| p.node_id == Some(node_id))
        .ok_or_else(|| ApiError::NotFound(format!("Node #{node_id} not found in cluster")))
}

/// Sends an authorized GET request to a peer's `/control/*` endpoint and deserializes JSON.
async fn fetch_peer_control_json<T: serde::de::DeserializeOwned>(
    state: &ApiState,
    peer_url: &str,
    path: &str,
) -> Result<T, ApiError> {
    let url = format!("{}{}", peer_url.trim().trim_end_matches('/'), path);
    let mut request = state.http_client.get(&url);
    if let Some(ref secret) = state.sidecar.cluster_secret {
        request = request.header("Authorization", make_auth_header_value(secret));
    }

    let response = request
        .send()
        .await
        .map_err(|e| ApiError::ServiceUnavailable(format!("Failed to reach peer control API: {e}")))?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(ApiError::Internal(format!(
            "Peer control endpoint returned {status}: {text}"
        )));
    }

    response
        .json::<T>()
        .await
        .map_err(|e| ApiError::Internal(format!("Invalid JSON from peer control endpoint: {e}")))
}

/// Handler for `GET /api/status`.
pub async fn get_status(State(state): State<ApiState>) -> Json<ClusterOverview> {
    Json(collect_cluster_overview(&state).await)
}

/// Handler for `GET /api/nodes`.
pub async fn get_nodes(State(state): State<ApiState>) -> Json<Vec<NodeSummary>> {
    let overview = collect_cluster_overview(&state).await;
    Json(overview.nodes)
}

/// Handler for `GET /api/nodes/:node_id/logs`.
pub async fn get_node_logs(
    State(state): State<ApiState>,
    Path(node_id): Path<u64>,
    Query(query): Query<LogsQuery>,
) -> Result<Json<NodeLogsResponse>, ApiError> {
    let limit = query.limit.unwrap_or(DEFAULT_LOG_LIMIT).clamp(1, MAX_LOG_LIMIT);
    if node_id == state.sidecar.node_id {
        let entries = state.sidecar.supervisor.recent_logs(limit).await;
        let total_buffered = state.sidecar.supervisor.total_buffered_logs().await;
        return Ok(Json(NodeLogsResponse {
            node_id,
            total_buffered,
            entries,
        }));
    }

    let peer = find_peer_by_node_id(&state, node_id).await?;
    let path = format!("/control/logs?limit={limit}");
    let logs: NodeLogsResponse = fetch_peer_control_json(&state, &peer.peer_url, &path).await?;
    Ok(Json(logs))
}

/// Handler for `GET /api/nodes/:node_id/config/:config_type`.
pub async fn get_node_config(
    State(state): State<ApiState>,
    Path((node_id, slug)): Path<(u64, String)>,
) -> Result<Json<NodeConfigResponse>, ApiError> {
    let config_type = NodeConfigType::from_slug(&slug).ok_or_else(|| {
        let supported: Vec<&str> = NodeConfigType::all().iter().map(|c| c.to_slug()).collect();
        ApiError::BadRequest(format!(
            "Invalid config file slug '{slug}'. Supported slugs: {supported:?}"
        ))
    })?;

    if node_id == state.sidecar.node_id {
        let mut resp = state
            .sidecar
            .supervisor
            .read_node_file(config_type)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to read config file: {e}")))?;
        resp.node_id = node_id;
        return Ok(Json(resp));
    }

    let peer = find_peer_by_node_id(&state, node_id).await?;
    let path = format!("/control/config/{}", config_type.to_slug());
    let cfg: NodeConfigResponse = fetch_peer_control_json(&state, &peer.peer_url, &path).await?;
    Ok(Json(cfg))
}
