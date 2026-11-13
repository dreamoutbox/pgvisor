use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::http::StatusCode;
use pgvisor_core::api::{ClusterMembersResponse, LoginRequest, LoginResponse};
use serde::de::DeserializeOwned;
use tokio::sync::RwLock;
use tracing::{debug, warn};

use super::error::SidecarClientError;
use super::sidecar_response::SidecarResponse;

pub const DEFAULT_SIDECAR_API_URL: &str = "http://127.0.0.1:8081";
pub const SIDECAR_URLS_ENV: &str = "PGVISOR_SIDECAR_URLS";
const SIDECAR_HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_NOT_LEADER_HOPS: usize = 3;

/// Parses a comma-separated list of sidecar API base URLs into trimmed, deduplicated URLs.
pub fn parse_sidecar_urls(raw: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut urls = Vec::new();
    for entry in raw.split(',') {
        let normalized = entry.trim().trim_end_matches('/');
        if !normalized.is_empty() && seen.insert(normalized.to_string()) {
            urls.push(normalized.to_string());
        }
    }
    urls
}

fn extract_error_message(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.get("error")
                .or_else(|| v.get("message"))
                .and_then(|s| s.as_str())
                .map(String::from)
        })
}

fn parse_not_leader_redirect(status: StatusCode, body: &[u8]) -> Option<Option<String>> {
    if status != StatusCode::CONFLICT {
        return None;
    }
    let val: serde_json::Value = serde_json::from_slice(body).ok()?;
    let is_not_leader = val.get("status").and_then(|s| s.as_str()) == Some("not_leader")
        || val.get("leader_url").is_some();
    if !is_not_leader {
        return None;
    }
    let leader_url = val
        .get("leader_url")
        .and_then(|s| s.as_str())
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty());
    Some(leader_url)
}

/// HTTP client communicating directly with `pgvisor-sidecar` `/api/*` endpoints,
/// featuring leader caching, automatic failover, and `409 NotLeader` redirection.
#[derive(Clone)]
pub struct SidecarClient {
    configured_urls: Vec<String>,
    cached_leader: Arc<RwLock<Option<String>>>,
    http: reqwest::Client,
}

impl SidecarClient {
    /// Creates a [`SidecarClient`] from an ordered list of sidecar API URLs (e.g. `http://pgvisor-node1:8081`).
    pub fn new(urls: Vec<String>) -> Result<Self, SidecarClientError> {
        let mut seen = HashSet::new();
        let cleaned: Vec<String> = urls
            .into_iter()
            .map(|u| u.trim().trim_end_matches('/').to_string())
            .filter(|u| !u.is_empty() && seen.insert(u.clone()))
            .collect();
        if cleaned.is_empty() {
            return Err(SidecarClientError::EmptyUrls);
        }

        let http = reqwest::Client::builder()
            .timeout(SIDECAR_HTTP_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| SidecarClientError::Unavailable(e.to_string()))?;

        Ok(Self {
            configured_urls: cleaned,
            cached_leader: Arc::new(RwLock::new(None)),
            http,
        })
    }

    /// Creates a [`SidecarClient`] from `PGVISOR_SIDECAR_URLS`, defaulting to `http://127.0.0.1:8081`.
    pub fn from_env() -> Result<Self, SidecarClientError> {
        let raw = std::env::var(SIDECAR_URLS_ENV)
            .unwrap_or_else(|_| DEFAULT_SIDECAR_API_URL.to_string());
        let urls = parse_sidecar_urls(&raw);
        Self::new(urls)
    }

    /// Returns the currently cached leader sidecar API URL, if any.
    pub async fn cached_leader(&self) -> Option<String> {
        self.cached_leader.read().await.clone()
    }

    /// Returns the configured list of sidecar API base URLs.
    pub fn configured_urls(&self) -> &[String] {
        &self.configured_urls
    }

    /// Queries `/api/cluster/members` to discover the active leader API URL and updates the cache.
    pub async fn discover_leader(&self, token: &str) -> Result<Option<String>, SidecarClientError> {
        let candidates = self.build_candidate_queue(false).await;
        let mut last_err = String::from("no sidecar responded");

        for base_url in candidates {
            let target = format!("{base_url}/api/cluster/members");
            let resp = match self
                .http
                .get(&target)
                .bearer_auth(token)
                .send()
                .await
            {
                Ok(r) => r,
                Err(err) => {
                    warn!(%target, ?err, "Leader discovery request failed");
                    last_err = err.to_string();
                    continue;
                }
            };

            let status = StatusCode::from_u16(resp.status().as_u16())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let bytes = resp.bytes().await.unwrap_or_default();

            if status == StatusCode::UNAUTHORIZED {
                return Err(SidecarClientError::Unauthorized);
            }
            if status == StatusCode::FORBIDDEN {
                let msg = extract_error_message(&bytes)
                    .unwrap_or_else(|| "user is not a PgVisor superuser".to_string());
                return Err(SidecarClientError::Forbidden(msg));
            }
            if !status.is_success() {
                last_err = format!("{target} returned {status}");
                continue;
            }

            if let Ok(parsed) = serde_json::from_slice::<ClusterMembersResponse>(&bytes) {
                let leader = parsed
                    .leader_url
                    .map(|u| u.trim().trim_end_matches('/').to_string())
                    .filter(|u| !u.is_empty());
                *self.cached_leader.write().await = leader.clone();
                debug!(?leader, "Updated cached leader URL from /api/cluster/members");
                return Ok(leader);
            }
        }

        Err(SidecarClientError::Unavailable(last_err))
    }

    async fn build_candidate_queue(&self, prioritize_cached_leader: bool) -> VecDeque<String> {
        let mut queue = VecDeque::new();
        let mut seen = HashSet::new();

        if prioritize_cached_leader {
            if let Some(leader) = self.cached_leader.read().await.clone() {
                if seen.insert(leader.clone()) {
                    queue.push_back(leader);
                }
            }
        }
        for url in &self.configured_urls {
            if seen.insert(url.clone()) {
                queue.push_back(url.clone());
            }
        }
        queue
    }

    async fn clear_cached_leader_if_matches(&self, base_url: &str) {
        let mut guard = self.cached_leader.write().await;
        if guard.as_deref() == Some(base_url) {
            *guard = None;
        }
    }

    /// Authenticates `username` and `password` against `/api/auth/login` across configured sidecars.
    pub async fn login(
        &self,
        username: &str,
        password: &str,
    ) -> Result<LoginResponse, SidecarClientError> {
        let payload = serde_json::to_vec(&LoginRequest {
            username: username.to_string(),
            password: password.to_string(),
        })
        .map_err(|e| SidecarClientError::Decode(e.to_string()))?;

        let resp = self
            .send_raw(
                false,
                reqwest::Method::POST,
                "/api/auth/login",
                None,
                Some("application/json"),
                Bytes::from(payload),
            )
            .await?;

        if resp.status.is_success() {
            serde_json::from_slice::<LoginResponse>(&resp.body)
                .map_err(|e| SidecarClientError::Decode(e.to_string()))
        } else {
            let msg = extract_error_message(&resp.body)
                .unwrap_or_else(|| format!("Login failed with HTTP {}", resp.status));
            Err(SidecarClientError::Upstream {
                status: resp.status,
                message: msg,
            })
        }
    }

    /// Sends a raw HTTP request to a sidecar `/api/*` endpoint with failover and leader re-discovery.
    pub async fn send_raw(
        &self,
        is_write: bool,
        method: reqwest::Method,
        path_and_query: &str,
        token: Option<&str>,
        content_type: Option<&str>,
        body: Bytes,
    ) -> Result<SidecarResponse, SidecarClientError> {
        let normalized_path = if path_and_query.starts_with('/') {
            path_and_query.to_string()
        } else {
            format!("/{path_and_query}")
        };

        if is_write && self.cached_leader.read().await.is_none() {
            if let Some(tok) = token {
                match self.discover_leader(tok).await {
                    Ok(_) => {}
                    Err(err @ (SidecarClientError::Unauthorized | SidecarClientError::Forbidden(_))) => {
                        return Err(err);
                    }
                    Err(err) => {
                        debug!(?err, "Initial leader discovery failed; falling back to configured URLs");
                    }
                }
            }
        }

        let mut queue = self.build_candidate_queue(is_write).await;
        let mut tried_servers = HashSet::new();
        let mut not_leader_hops = 0usize;
        let mut discovered_after_failure = false;
        let mut last_error = String::from("no reachable sidecar endpoint");

        while let Some(base_url) = queue.pop_front() {
            if !tried_servers.insert(base_url.clone()) {
                continue;
            }

            let target = format!("{base_url}{normalized_path}");
            let mut req = self.http.request(method.clone(), &target);
            if let Some(tok) = token {
                req = req.bearer_auth(tok);
            }
            if let Some(ct) = content_type {
                req = req.header(reqwest::header::CONTENT_TYPE, ct);
            }
            if !body.is_empty() {
                req = req.body(body.clone());
            }

            let resp = match req.send().await {
                Ok(r) => r,
                Err(err) => {
                    warn!(%target, ?err, "Sidecar API connection error; failing over");
                    last_error = err.to_string();
                    self.clear_cached_leader_if_matches(&base_url).await;
                    if is_write && !discovered_after_failure {
                        if let Some(tok) = token {
                            discovered_after_failure = true;
                            if let Ok(Some(new_leader)) = self.discover_leader(tok).await {
                                if !tried_servers.contains(&new_leader) {
                                    queue.push_front(new_leader);
                                }
                            }
                        }
                    }
                    continue;
                }
            };

            let status = StatusCode::from_u16(resp.status().as_u16())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let resp_content_type = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(String::from);
            let resp_content_disposition = resp
                .headers()
                .get(reqwest::header::CONTENT_DISPOSITION)
                .and_then(|v| v.to_str().ok())
                .map(String::from);
            let resp_bytes = resp.bytes().await.unwrap_or_default();

            if status == StatusCode::UNAUTHORIZED {
                return Err(SidecarClientError::Unauthorized);
            }
            if status == StatusCode::FORBIDDEN {
                let msg = extract_error_message(&resp_bytes)
                    .unwrap_or_else(|| "user is not a PgVisor superuser".to_string());
                return Err(SidecarClientError::Forbidden(msg));
            }

            if let Some(leader_hint) = parse_not_leader_redirect(status, &resp_bytes) {
                if not_leader_hops < MAX_NOT_LEADER_HOPS {
                    not_leader_hops += 1;
                    self.clear_cached_leader_if_matches(&base_url).await;
                    if let Some(new_leader) = leader_hint {
                        *self.cached_leader.write().await = Some(new_leader.clone());
                        tried_servers.remove(&new_leader);
                        queue.push_front(new_leader);
                        continue;
                    }
                    if let Some(tok) = token {
                        if let Ok(Some(discovered)) = self.discover_leader(tok).await {
                            tried_servers.remove(&discovered);
                            queue.push_front(discovered);
                            continue;
                        }
                    }
                    continue;
                }
            }

            if status.is_server_error() && (status == StatusCode::BAD_GATEWAY || status == StatusCode::SERVICE_UNAVAILABLE || status == StatusCode::GATEWAY_TIMEOUT) {
                warn!(%target, %status, "Sidecar returned transient gateway/availability error; trying next URL");
                last_error = format!("{target} returned {status}");
                self.clear_cached_leader_if_matches(&base_url).await;
                continue;
            }

            if is_write && status.is_success() {
                *self.cached_leader.write().await = Some(base_url);
            }

            return Ok(SidecarResponse {
                status,
                content_type: resp_content_type,
                content_disposition: resp_content_disposition,
                body: resp_bytes,
            });
        }

        Err(SidecarClientError::Unavailable(last_error))
    }

    /// Executes a GET JSON call against any healthy sidecar API node.
    pub async fn get_json<T: DeserializeOwned>(
        &self,
        path_and_query: &str,
        token: &str,
    ) -> Result<T, SidecarClientError> {
        let resp = self
            .send_raw(
                false,
                reqwest::Method::GET,
                path_and_query,
                Some(token),
                None,
                Bytes::new(),
            )
            .await?;

        if resp.status == StatusCode::NOT_FOUND {
            let msg = extract_error_message(&resp.body)
                .unwrap_or_else(|| format!("{path_and_query} not found"));
            return Err(SidecarClientError::NotFound(msg));
        }
        if !resp.status.is_success() {
            let msg = extract_error_message(&resp.body)
                .unwrap_or_else(|| String::from_utf8_lossy(&resp.body).to_string());
            return Err(SidecarClientError::Upstream {
                status: resp.status,
                message: msg,
            });
        }

        serde_json::from_slice::<T>(&resp.body)
            .map_err(|e| SidecarClientError::Decode(format!("{path_and_query}: {e}")))
    }
}
