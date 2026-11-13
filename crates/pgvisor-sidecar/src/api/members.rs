use std::time::Duration;

use axum::extract::State;
use axum::Json;
use pgvisor_core::api::{ClusterMember, ClusterMembersResponse, MemberHealth, SidecarRole};
use pgvisor_core::auth::make_auth_header_value;
use reqwest::Url;
use tokio::task::JoinSet;
use tracing::debug;

use super::api_state::ApiState;
use super::error::ApiError;
use crate::control::StatusResponse;

const PEER_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Builds a sidecar's API base URL from its peer control URL and reported `api_port`.
///
/// # Arguments
/// * `peer_url` - Control URL from `PGVISOR_PEERS` (e.g. `http://pgvisor-node2:8080`).
/// * `api_port` - Client API port reported by `/control/status`.
pub fn build_api_url_from_peer(peer_url: &str, api_port: u16) -> Option<String> {
    let mut url = Url::parse(peer_url.trim()).ok()?;
    url.set_port(Some(api_port)).ok()?;
    url.set_path("");
    url.set_query(None);
    url.set_fragment(None);
    Some(url.to_string().trim_end_matches('/').to_string())
}

/// Extracts a numeric node identifier from peer hostnames formatted like `pgvisor-node2` or `node2`.
///
/// # Arguments
/// * `peer_url` - Peer URL string from `PGVISOR_PEERS`.
pub fn parse_node_id_from_peer(peer_url: &str) -> Option<u64> {
    let url = Url::parse(peer_url.trim()).ok()?;
    let host = url.host_str()?;
    let first_label = host.split('.').next()?;
    let digits = first_label
        .strip_prefix("pgvisor-node")
        .or_else(|| first_label.strip_prefix("node"))?;
    digits.parse::<u64>().ok()
}

/// Returns true if `peer_url` identifies the local sidecar node by hostname convention.
fn is_self_peer_url(peer_url: &str, self_node_id: u64) -> bool {
    parse_node_id_from_peer(peer_url) == Some(self_node_id)
}

/// Fetches `/control/status` from a single peer with a short timeout and optional cluster auth.
pub(crate) async fn fetch_peer_status(
    client: &reqwest::Client,
    peer_url: &str,
    cluster_secret: Option<&str>,
) -> Option<StatusResponse> {
    let base = peer_url.trim().trim_end_matches('/');
    let url = format!("{base}/control/status");
    let mut request = client.get(&url).timeout(PEER_PROBE_TIMEOUT);
    if let Some(secret) = cluster_secret {
        request = request.header("Authorization", make_auth_header_value(secret));
    }

    let response = request.send().await.ok()?;
    if !response.status().is_success() {
        debug!(peer = %peer_url, status = %response.status(), "Peer status probe failed");
        return None;
    }
    response.json::<StatusResponse>().await.ok()
}

/// Fans out to `PGVISOR_PEERS` in parallel and combines peer state with local sidecar state.
///
/// # Arguments
/// * `state` - Shared API state containing local sidecar state and configured peers.
pub async fn collect_cluster_members(state: &ApiState) -> ClusterMembersResponse {
    let self_node_id = state.sidecar.node_id;
    let self_role = state.sidecar.current_role().await;
    let self_api_port = state.sidecar.api_port;

    let mut members = Vec::new();
    let mut self_seen = false;
    let mut join_set: JoinSet<(String, Option<StatusResponse>)> = JoinSet::new();

    for peer_url in state.sidecar.peers.iter() {
        if is_self_peer_url(peer_url, self_node_id) {
            if !self_seen {
                self_seen = true;
                members.push(ClusterMember {
                    node_id: Some(self_node_id),
                    api_url: build_api_url_from_peer(peer_url, self_api_port),
                    role: Some(self_role),
                    health: MemberHealth::Healthy,
                    is_self: true,
                });
            }
            continue;
        }

        let client = state.http_client.clone();
        let url = peer_url.clone();
        let secret = state.sidecar.cluster_secret.clone();
        join_set.spawn(async move {
            let status = fetch_peer_status(&client, &url, secret.as_deref()).await;
            (url, status)
        });
    }

    while let Some(joined) = join_set.join_next().await {
        let Ok((peer_url, status_opt)) = joined else {
            continue;
        };

        match status_opt {
            Some(status) if status.node_id == self_node_id => {
                if !self_seen {
                    self_seen = true;
                    members.push(ClusterMember {
                        node_id: Some(self_node_id),
                        api_url: build_api_url_from_peer(&peer_url, self_api_port),
                        role: Some(self_role),
                        health: MemberHealth::Healthy,
                        is_self: true,
                    });
                }
            }
            Some(status) => {
                members.push(ClusterMember {
                    node_id: Some(status.node_id),
                    api_url: build_api_url_from_peer(&peer_url, status.api_port),
                    role: Some(SidecarRole::from_role_str(&status.role)),
                    health: MemberHealth::Healthy,
                    is_self: false,
                });
            }
            None => {
                members.push(ClusterMember {
                    node_id: parse_node_id_from_peer(&peer_url),
                    api_url: None,
                    role: None,
                    health: MemberHealth::Unreachable,
                    is_self: false,
                });
            }
        }
    }

    if !self_seen {
        members.push(ClusterMember {
            node_id: Some(self_node_id),
            api_url: Some(format!("http://127.0.0.1:{self_api_port}")),
            role: Some(self_role),
            health: MemberHealth::Healthy,
            is_self: true,
        });
    }

    members.sort_by(|a, b| {
        a.node_id
            .unwrap_or(u64::MAX)
            .cmp(&b.node_id.unwrap_or(u64::MAX))
            .then_with(|| a.api_url.cmp(&b.api_url))
    });

    // Prefer a healthy leader; if somehow multiple claim leader, prefer self.
    let leader_url = members
        .iter()
        .find(|m| {
            m.is_self && m.health == MemberHealth::Healthy && m.role == Some(SidecarRole::Leader)
        })
        .or_else(|| {
            members
                .iter()
                .find(|m| m.health == MemberHealth::Healthy && m.role == Some(SidecarRole::Leader))
        })
        .and_then(|m| m.api_url.clone());

    ClusterMembersResponse {
        leader_url,
        members,
    }
}

/// Ensures the handling sidecar is currently the cluster leader; otherwise returns HTTP 409
/// with the discovered `leader_url`.
///
/// # Arguments
/// * `state` - Shared API state.
pub async fn require_leader(state: &ApiState) -> Result<(), ApiError> {
    if state.sidecar.current_role().await == SidecarRole::Leader {
        return Ok(());
    }
    let snapshot = collect_cluster_members(state).await;
    Err(ApiError::NotLeader {
        leader_url: snapshot.leader_url,
    })
}

/// Handler for `GET /api/cluster/members`.
pub async fn get_cluster_members(
    State(state): State<ApiState>,
) -> Result<Json<ClusterMembersResponse>, ApiError> {
    Ok(Json(collect_cluster_members(&state).await))
}
