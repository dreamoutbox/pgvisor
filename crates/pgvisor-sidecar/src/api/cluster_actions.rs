use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::Json;
use pgvisor_core::api::{
    NodeActionRequest, NodeActionResponse, NodeLifecycleAction, SidecarRole, SwitchoverRequest,
    SwitchoverResponse,
};
use pgvisor_core::audit::AuditEventKind;
use pgvisor_core::auth::make_auth_header_value;
use tracing::{info, warn};

use super::api_state::ApiState;
use super::authenticated_user::AuthenticatedUser;
use super::cluster::find_peer_by_node_id;
use super::error::ApiError;
use super::members::require_leader;
use super::peer_probe::{probe_all_peers, PeerProbe};
use crate::control::{
    handle_demote, handle_promote, handle_repoint, handle_restart, handle_start, handle_stop,
    RepointPayload,
};

fn map_control_err((status, Json(body)): (StatusCode, Json<serde_json::Value>)) -> ApiError {
    let msg = body
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("Sidecar control operation failed")
        .to_string();
    match status {
        StatusCode::BAD_REQUEST => ApiError::BadRequest(msg),
        StatusCode::NOT_FOUND => ApiError::NotFound(msg),
        StatusCode::FORBIDDEN => ApiError::Forbidden(msg),
        StatusCode::UNAUTHORIZED => ApiError::Unauthorized(msg),
        _ => ApiError::Internal(msg),
    }
}

/// Sends an authorized POST request to a peer's `/control/*` endpoint.
pub(crate) async fn post_peer_control(
    state: &ApiState,
    peer_url: &str,
    path: &str,
    payload: Option<serde_json::Value>,
) -> Result<serde_json::Value, ApiError> {
    let url = format!("{}{}", peer_url.trim().trim_end_matches('/'), path);
    let mut request = state.http_client.post(&url);
    if let Some(ref secret) = state.sidecar.cluster_secret {
        request = request.header("Authorization", make_auth_header_value(secret));
    }
    if let Some(body) = payload {
        request = request.json(&body);
    }

    let response = request.send().await.map_err(|e| {
        ApiError::ServiceUnavailable(format!("Failed to contact peer control endpoint: {e}"))
    })?;

    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        let err_msg = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(ToString::to_string))
            .unwrap_or(text);
        return if status == StatusCode::BAD_REQUEST {
            Err(ApiError::BadRequest(err_msg))
        } else {
            Err(ApiError::Internal(format!(
                "Peer control endpoint {url} returned {status}: {err_msg}"
            )))
        };
    }

    response
        .json::<serde_json::Value>()
        .await
        .or_else(|_| Ok(serde_json::json!({ "status": "ok" })))
}

async fn run_node_lifecycle_action(
    state: &ApiState,
    user: &AuthenticatedUser,
    node_id: u64,
    action: NodeLifecycleAction,
) -> Result<NodeActionResponse, ApiError> {
    require_leader(state).await?;

    let peer = find_peer_by_node_id(state, node_id).await?;
    let status = peer.status.as_ref().ok_or_else(|| {
        ApiError::ServiceUnavailable(format!("Node #{node_id} sidecar is unreachable"))
    })?;

    match action {
        NodeLifecycleAction::Start if status.status == "running" => {
            return Err(ApiError::BadRequest(format!(
                "Node #{node_id} is already running"
            )));
        }
        NodeLifecycleAction::Stop if status.status == "stopped" => {
            return Err(ApiError::BadRequest(format!(
                "Node #{node_id} is already stopped"
            )));
        }
        _ => {}
    }

    info!(
        actor = %user.username,
        node_id,
        ?action,
        "Executing node lifecycle action on cluster"
    );

    if peer.is_self {
        let res = match action {
            NodeLifecycleAction::Start => handle_start(State(state.sidecar.clone())).await,
            NodeLifecycleAction::Stop => handle_stop(State(state.sidecar.clone())).await,
            NodeLifecycleAction::Restart => handle_restart(State(state.sidecar.clone())).await,
        };
        // Success status is verified via `?`; response payload is only informational.
        let _control_resp = res.map_err(map_control_err)?;
    } else {
        let path = match action {
            NodeLifecycleAction::Start => "/control/start",
            NodeLifecycleAction::Stop => "/control/stop",
            NodeLifecycleAction::Restart => "/control/restart",
        };
        post_peer_control(state, &peer.peer_url, path, None).await?;
    }

    let (kind, verb) = match action {
        NodeLifecycleAction::Start => (AuditEventKind::NodeUp, "started"),
        NodeLifecycleAction::Stop => (AuditEventKind::NodeDown, "stopped"),
        NodeLifecycleAction::Restart => (AuditEventKind::NodeUp, "restarted"),
    };
    state
        .audit_log
        .append(
            kind,
            Some(node_id),
            Some(&peer.pg_address),
            format!("Node #{node_id} {verb} by actor '{}'", user.username),
            None,
        )
        .await;

    Ok(NodeActionResponse {
        status: "ok".into(),
        message: format!("Node #{node_id} {verb} successfully"),
        node_id,
        action,
    })
}

async fn repoint_peer(
    state: &ApiState,
    peer: &PeerProbe,
    primary_conninfo: &str,
) -> Result<(), ApiError> {
    if peer.is_self {
        handle_repoint(
            State(state.sidecar.clone()),
            Json(RepointPayload {
                primary_conninfo: primary_conninfo.to_string(),
            }),
        )
        .await
        .map(|_| ())
        .map_err(map_control_err)
    } else {
        post_peer_control(
            state,
            &peer.peer_url,
            "/control/repoint",
            Some(serde_json::json!({ "primary_conninfo": primary_conninfo })),
        )
        .await
        .map(|_| ())
    }
}

/// Handler for `POST /api/nodes/:node_id/start`.
pub async fn post_start_node(
    State(state): State<ApiState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(node_id): Path<u64>,
) -> Result<Json<NodeActionResponse>, ApiError> {
    Ok(Json(
        run_node_lifecycle_action(&state, &user, node_id, NodeLifecycleAction::Start).await?,
    ))
}

/// Handler for `POST /api/nodes/:node_id/stop`.
pub async fn post_stop_node(
    State(state): State<ApiState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(node_id): Path<u64>,
) -> Result<Json<NodeActionResponse>, ApiError> {
    Ok(Json(
        run_node_lifecycle_action(&state, &user, node_id, NodeLifecycleAction::Stop).await?,
    ))
}

/// Handler for `POST /api/nodes/:node_id/restart`.
pub async fn post_restart_node(
    State(state): State<ApiState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(node_id): Path<u64>,
) -> Result<Json<NodeActionResponse>, ApiError> {
    Ok(Json(
        run_node_lifecycle_action(&state, &user, node_id, NodeLifecycleAction::Restart).await?,
    ))
}

/// Handler for `POST /api/nodes/:node_id/action`.
pub async fn post_node_action(
    State(state): State<ApiState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(node_id): Path<u64>,
    Json(payload): Json<NodeActionRequest>,
) -> Result<Json<NodeActionResponse>, ApiError> {
    Ok(Json(
        run_node_lifecycle_action(&state, &user, node_id, payload.action).await?,
    ))
}

/// Handler for `POST /api/nodes/:node_id/repoint` (leader-only standby repoint).
pub async fn post_repoint_node(
    State(state): State<ApiState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(node_id): Path<u64>,
    Json(payload): Json<RepointPayload>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_leader(&state).await?;
    let peer = find_peer_by_node_id(&state, node_id).await?;
    repoint_peer(&state, &peer, &payload.primary_conninfo).await?;

    state
        .audit_log
        .append(
            AuditEventKind::NodeJoined,
            Some(node_id),
            Some(&peer.pg_address),
            format!(
                "Standby node #{node_id} repointed to '{}' by actor '{}'",
                payload.primary_conninfo, user.username
            ),
            None,
        )
        .await;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": format!("Node #{node_id} repointed to {}", payload.primary_conninfo),
        "node_id": node_id,
    })))
}

/// Handler for `POST /api/cluster/switchover` (leader-only).
pub async fn post_switchover(
    State(state): State<ApiState>,
    Extension(user): Extension<AuthenticatedUser>,
    Json(payload): Json<SwitchoverRequest>,
) -> Result<Json<SwitchoverResponse>, ApiError> {
    require_leader(&state).await?;
    let target_node_id = payload.target_node_id;

    let probes = probe_all_peers(&state).await;
    let target_peer = probes
        .iter()
        .find(|p| p.node_id == Some(target_node_id))
        .ok_or_else(|| {
            ApiError::NotFound(format!("Target node #{target_node_id} does not exist in cluster"))
        })?;

    let target_status = target_peer.status.as_ref().ok_or_else(|| {
        ApiError::BadRequest(format!("Target node #{target_node_id} is unreachable"))
    })?;
    if SidecarRole::from_role_str(&target_status.role) == SidecarRole::Leader {
        return Err(ApiError::BadRequest(format!(
            "Node #{target_node_id} is already the active leader"
        )));
    }
    if target_status.status != "running" {
        return Err(ApiError::BadRequest(format!(
            "Target node #{target_node_id} is not running (current status: {})",
            target_status.status
        )));
    }

    let current_leader = probes.iter().find(|p| {
        p.status
            .as_ref()
            .is_some_and(|s| SidecarRole::from_role_str(&s.role) == SidecarRole::Leader)
    });
    let previous_leader_id = current_leader.and_then(|p| p.node_id);

    info!(
        actor = %user.username,
        target_node_id,
        ?previous_leader_id,
        "Initiating cluster leader switchover"
    );

    // 1. Gracefully demote active leader
    if let Some(leader_peer) = current_leader {
        if leader_peer.is_self {
            if let Err(err) = handle_demote(State(state.sidecar.clone())).await {
                warn!(?err, "Local leader demote returned error; proceeding with promotion");
            }
        } else if let Err(err) = post_peer_control(&state, &leader_peer.peer_url, "/control/demote", None).await {
            warn!(?err, "Remote leader demote returned error; proceeding with promotion");
        }
    }

    // 2. Promote target standby
    if target_peer.is_self {
        // Success status is verified via `?`; response payload is only informational.
        let _control_resp = handle_promote(State(state.sidecar.clone()))
            .await
            .map_err(map_control_err)?;
    } else {
        post_peer_control(&state, &target_peer.peer_url, "/control/promote", None).await?;
    }

    // 3. Repoint remaining standby nodes to the newly promoted primary
    let new_primary_host = target_peer
        .pg_address
        .split(':')
        .next()
        .unwrap_or("127.0.0.1");
    let new_conninfo = format!(
        "host={new_primary_host} port=5432 user={}",
        state.pg_superuser
    );
    for peer in &probes {
        if peer.node_id == Some(target_node_id) {
            continue;
        }
        let is_standby = peer
            .role()
            .is_some_and(|r| r == SidecarRole::Standby || r == SidecarRole::Fenced);
        if is_standby {
            if let Err(err) = repoint_peer(&state, peer, &new_conninfo).await {
                warn!(
                    node_id = ?peer.node_id,
                    ?err,
                    "Failed to repoint standby after switchover"
                );
            }
        }
    }

    state
        .audit_log
        .append(
            AuditEventKind::ElectionResult,
            Some(target_node_id),
            Some(&target_peer.pg_address),
            format!(
                "Manual switchover completed by actor '{}': Node #{} promoted to leader (previous leader: {:?})",
                user.username, target_node_id, previous_leader_id
            ),
            None,
        )
        .await;

    Ok(Json(SwitchoverResponse {
        status: "ok".into(),
        message: format!("Switched over leader to Node #{target_node_id}"),
        previous_leader_id,
        new_leader_id: target_node_id,
    }))
}
