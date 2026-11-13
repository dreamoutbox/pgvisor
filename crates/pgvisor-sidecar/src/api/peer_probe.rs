use std::time::Duration;

use pgvisor_core::api::{MemberHealth, SidecarRole};
use pgvisor_core::auth::make_auth_header_value;
use reqwest::Url;
use tokio::task::JoinSet;
use tracing::debug;

use super::api_state::ApiState;
use super::members::{build_api_url_from_peer, parse_node_id_from_peer};
use crate::control::{collect_local_status, StatusResponse};

const PEER_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Combined status snapshot of one sidecar peer (local or remote) used across API handlers.
#[derive(Debug, Clone)]
pub struct PeerProbe {
    pub peer_url: String,
    pub is_self: bool,
    pub node_id: Option<u64>,
    pub pg_address: String,
    pub api_url: Option<String>,
    pub status: Option<StatusResponse>,
}

impl PeerProbe {
    /// Returns the typed operational role reported by the peer when reachable.
    pub fn role(&self) -> Option<SidecarRole> {
        self.status
            .as_ref()
            .map(|s| SidecarRole::from_role_str(&s.role))
    }

    /// Returns reachability of this peer.
    pub fn health(&self) -> MemberHealth {
        if self.status.is_some() {
            MemberHealth::Healthy
        } else {
            MemberHealth::Unreachable
        }
    }
}

/// Derives a PostgreSQL endpoint string (`host:5432`) from a peer control URL.
pub fn pg_address_from_peer_url(peer_url: &str, default_port: u16) -> String {
    Url::parse(peer_url.trim())
        .ok()
        .and_then(|u| u.host_str().map(|h| format!("{h}:{default_port}")))
        .unwrap_or_else(|| peer_url.trim().to_string())
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

/// Probes all configured `PGVISOR_PEERS` in parallel and uses local state for self without HTTP loopback.
///
/// # Arguments
/// * `state` - Shared sidecar API state.
pub async fn probe_all_peers(state: &ApiState) -> Vec<PeerProbe> {
    let self_node_id = state.sidecar.node_id;
    let self_api_port = state.sidecar.api_port;
    let self_pg_port = state.sidecar.pg_port;
    let local_status = collect_local_status(&state.sidecar).await;

    let mut probes = Vec::new();
    let mut self_seen = false;
    let mut join_set: JoinSet<(String, Option<StatusResponse>)> = JoinSet::new();

    for peer_url in state.sidecar.peers.iter() {
        if parse_node_id_from_peer(peer_url) == Some(self_node_id) {
            if !self_seen {
                self_seen = true;
                probes.push(PeerProbe {
                    peer_url: peer_url.clone(),
                    is_self: true,
                    node_id: Some(self_node_id),
                    pg_address: pg_address_from_peer_url(peer_url, self_pg_port),
                    api_url: build_api_url_from_peer(peer_url, self_api_port),
                    status: Some(local_status.clone()),
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
                    probes.push(PeerProbe {
                        peer_url: peer_url.clone(),
                        is_self: true,
                        node_id: Some(self_node_id),
                        pg_address: pg_address_from_peer_url(&peer_url, self_pg_port),
                        api_url: build_api_url_from_peer(&peer_url, self_api_port),
                        status: Some(local_status.clone()),
                    });
                }
            }
            Some(status) => {
                let api_url = build_api_url_from_peer(&peer_url, status.api_port);
                let pg_address = pg_address_from_peer_url(&peer_url, 5432);
                probes.push(PeerProbe {
                    peer_url,
                    is_self: false,
                    node_id: Some(status.node_id),
                    pg_address,
                    api_url,
                    status: Some(status),
                });
            }
            None => {
                let node_id = parse_node_id_from_peer(&peer_url);
                let pg_address = pg_address_from_peer_url(&peer_url, 5432);
                probes.push(PeerProbe {
                    peer_url,
                    is_self: false,
                    node_id,
                    pg_address,
                    api_url: None,
                    status: None,
                });
            }
        }
    }

    if !self_seen {
        probes.push(PeerProbe {
            peer_url: format!("http://127.0.0.1:{}", self_api_port),
            is_self: true,
            node_id: Some(self_node_id),
            pg_address: state.pg_addr.clone(),
            api_url: Some(format!("http://127.0.0.1:{self_api_port}")),
            status: Some(local_status),
        });
    }

    probes.sort_by(|a, b| {
        a.node_id
            .unwrap_or(u64::MAX)
            .cmp(&b.node_id.unwrap_or(u64::MAX))
            .then_with(|| a.api_url.cmp(&b.api_url))
            .then_with(|| a.peer_url.cmp(&b.peer_url))
    });

    probes
}
