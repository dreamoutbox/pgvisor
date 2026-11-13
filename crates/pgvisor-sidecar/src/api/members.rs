use axum::extract::State;
use axum::Json;
use pgvisor_core::api::{ClusterMember, ClusterMembersResponse, MemberHealth, SidecarRole};
use reqwest::Url;

use super::api_state::ApiState;
use super::error::ApiError;
use super::peer_probe::probe_all_peers;

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

/// Fans out to `PGVISOR_PEERS` in parallel and combines peer state with local sidecar state.
///
/// # Arguments
/// * `state` - Shared API state containing local sidecar state and configured peers.
pub async fn collect_cluster_members(state: &ApiState) -> ClusterMembersResponse {
    let probes = probe_all_peers(state).await;

    let members: Vec<ClusterMember> = probes
        .into_iter()
        .map(|probe| {
            let role = probe.role();
            let health = probe.health();
            ClusterMember {
                node_id: probe.node_id,
                api_url: probe.api_url,
                role,
                health,
                is_self: probe.is_self,
            }
        })
        .collect();

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
