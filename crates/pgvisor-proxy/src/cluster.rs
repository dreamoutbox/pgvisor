use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

use pgvisor_core::audit::{AuditEventKind, AuditLog};
use pgvisor_dashboard::handlers::ClusterService;
use pgvisor_dashboard::models::{NodeActionResponse, NodeLifecycleAction, SwitchoverResponse};

use crate::pool::ConnectionPool;

/// Registered PostgreSQL cluster target endpoint.
#[derive(Clone, Debug)]
pub struct NodeTarget {
    pub pg_addr: String,
    pub control_url: String,
    pub is_dynamic: bool,
    pub consecutive_failures: usize,
}

/// Production implementation of ClusterService orchestrating live PostgreSQL switchovers.
pub struct ProxyClusterService {
    targets: Arc<RwLock<Vec<NodeTarget>>>,
    leader_addr: Arc<RwLock<Option<String>>>,
    standby_addrs: Arc<RwLock<Vec<String>>>,
    pool: ConnectionPool,
    http_client: reqwest::Client,
    audit_log: Option<Arc<AuditLog>>,
    cluster_secret: Option<String>,
}

impl ProxyClusterService {
    pub fn new(
        targets: Arc<RwLock<Vec<NodeTarget>>>,
        leader_addr: Arc<RwLock<Option<String>>>,
        standby_addrs: Arc<RwLock<Vec<String>>>,
        pool: ConnectionPool,
    ) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            targets,
            leader_addr,
            standby_addrs,
            pool,
            http_client,
            audit_log: None,
            cluster_secret: None,
        }
    }

    /// Injects central audit log store into cluster service.
    pub fn with_audit_log(mut self, audit_log: Arc<AuditLog>) -> Self {
        self.audit_log = Some(audit_log);
        self
    }

    /// Injects cluster shared secret for authenticating requests to sidecars.
    pub fn with_cluster_secret(mut self, cluster_secret: Option<String>) -> Self {
        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(secret) = cluster_secret.as_deref() {
            if let Ok(val) = reqwest::header::HeaderValue::from_str(
                &pgvisor_core::auth::make_auth_header_value(secret),
            ) {
                headers.insert(reqwest::header::AUTHORIZATION, val);
            }
        }
        self.http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .default_headers(headers)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        self.cluster_secret = cluster_secret;
        self
    }

    /// Finds target node by node_id through querying its sidecar /control/status.
    async fn find_target(&self, node_id: u64) -> Result<(NodeTarget, NodeStatusResponse), String> {
        let targets = {
            let t = self.targets.read().await;
            t.clone()
        };

        for target in &targets {
            let status_url = format!(
                "{}/control/status",
                target.control_url.trim_end_matches('/')
            );
            if let Ok(resp) = self.http_client.get(&status_url).send().await {
                if let Ok(st) = resp.json::<NodeStatusResponse>().await {
                    if st.node_id == node_id {
                        return Ok((target.clone(), st));
                    }
                }
            }
        }

        Err(format!(
            "Node #{} not found or sidecar not reachable",
            node_id
        ))
    }
}

#[derive(serde::Deserialize)]
struct NodeStatusResponse {
    node_id: u64,
    role: String,
    status: String,
}

#[async_trait::async_trait]
impl ClusterService for ProxyClusterService {
    async fn switchover(&self, target_node_id: u64) -> Result<SwitchoverResponse, String> {
        info!(target_node_id, "Executing manual leader switchover");

        // 1. Inspect all targets to discover current leader and validate target standby
        let targets = {
            let t = self.targets.read().await;
            t.clone()
        };

        let mut target_control_url: Option<String> = None;
        let mut target_pg_addr: Option<String> = None;
        let mut current_leader_control_url: Option<String> = None;
        let mut current_leader_id: Option<u64> = None;
        let mut standby_control_urls: Vec<(u64, String)> = Vec::new();

        for target in &targets {
            let status_url = format!(
                "{}/control/status",
                target.control_url.trim_end_matches('/')
            );
            if let Ok(resp) = self.http_client.get(&status_url).send().await {
                if let Ok(st) = resp.json::<NodeStatusResponse>().await {
                    if st.node_id == target_node_id {
                        if st.role == "leader" {
                            return Err(format!(
                                "Node #{} is already the active leader",
                                target_node_id
                            ));
                        }
                        if st.status != "running" {
                            return Err(format!(
                                "Target node #{} is not running (status: {})",
                                target_node_id, st.status
                            ));
                        }
                        target_control_url = Some(target.control_url.clone());
                        target_pg_addr = Some(target.pg_addr.clone());
                    } else if st.role == "leader" && st.status == "running" {
                        current_leader_control_url = Some(target.control_url.clone());
                        current_leader_id = Some(st.node_id);
                    } else if st.role == "standby" {
                        standby_control_urls.push((st.node_id, target.control_url.clone()));
                    }
                }
            }
        }

        let target_url = target_control_url.ok_or_else(|| {
            format!(
                "Target node #{} not found or not reachable among cluster nodes",
                target_node_id
            )
        })?;
        let new_leader_pg = target_pg_addr
            .ok_or_else(|| format!("Target node #{} address not found", target_node_id))?;

        // 2. Step 1: Gracefully demote current leader (if active)
        if let Some(ref leader_url) = current_leader_control_url {
            let demote_url = format!("{}/control/demote", leader_url.trim_end_matches('/'));
            info!(%demote_url, leader_id = ?current_leader_id, "Demoting current leader");
            let resp = self
                .http_client
                .post(&demote_url)
                .send()
                .await
                .map_err(|e| {
                    format!("Failed to contact current leader at {}: {}", demote_url, e)
                })?;

            if !resp.status().is_success() {
                let err_body = resp.text().await.unwrap_or_default();
                warn!(
                    %err_body,
                    "Leader demote returned non-success status; proceeding with promotion"
                );
            }
        }

        // 3. Step 2: Promote target standby to leader
        let promote_url = format!("{}/control/promote", target_url.trim_end_matches('/'));
        info!(%promote_url, target_node_id, "Promoting target node to leader");
        let resp = self
            .http_client
            .post(&promote_url)
            .send()
            .await
            .map_err(|e| format!("Failed to send promote command to {}: {}", promote_url, e))?;

        if !resp.status().is_success() {
            let err_body = resp.text().await.unwrap_or_default();
            return Err(format!(
                "Promote failed on Node #{}: {}",
                target_node_id, err_body
            ));
        }

        // 4. Step 3: Repoint remaining standby nodes to the new leader
        let new_primary_host = new_leader_pg.split(':').next().unwrap_or("127.0.0.1");
        let new_conninfo = format!("host={} port=5432 user=postgres", new_primary_host);
        for (sid, s_url) in standby_control_urls {
            if sid == target_node_id {
                continue;
            }
            let repoint_url = format!("{}/control/repoint", s_url.trim_end_matches('/'));
            info!(sid, %repoint_url, %new_conninfo, "Repointing standby to new primary");
            let payload = serde_json::json!({
                "primary_conninfo": new_conninfo
            });
            let _ = self
                .http_client
                .post(&repoint_url)
                .json(&payload)
                .send()
                .await;
        }

        // 5. Step 4: Update proxy connection pool and topology
        let mut new_standbys = Vec::new();
        for t in &targets {
            if t.pg_addr != new_leader_pg {
                new_standbys.push(t.pg_addr.clone());
            }
        }

        {
            let mut l = self.leader_addr.write().await;
            *l = Some(new_leader_pg.clone());
        }
        {
            let mut s = self.standby_addrs.write().await;
            *s = new_standbys.clone();
        }

        self.pool.drain_all().await;
        self.pool
            .update_topology(Some(new_leader_pg.clone()), new_standbys)
            .await;

        info!(
            previous_leader = ?current_leader_id,
            new_leader = target_node_id,
            new_addr = %new_leader_pg,
            "Leader switchover completed successfully"
        );

        if let Some(audit) = self.audit_log.as_ref() {
            let detail = format!(
                "Manual switchover completed: Node #{} promoted to leader (previous leader: {:?})",
                target_node_id, current_leader_id
            );
            audit
                .append(
                    AuditEventKind::ElectionResult,
                    Some(target_node_id),
                    Some(&new_leader_pg),
                    detail,
                    None,
                )
                .await;
        }

        let old_node = current_leader_id
            .map(|id| format!("node{}", id))
            .unwrap_or_else(|| "node1".to_string());
        let new_node = format!("node{}", target_node_id);
        pgvisor_core::log_highlight(&pgvisor_core::format_leader_down_highlight(
            &old_node, &new_node,
        ));

        Ok(SwitchoverResponse {
            status: "ok".into(),
            message: format!(
                "Successfully switched over leader to Node #{}",
                target_node_id
            ),
            previous_leader_id: current_leader_id,
            new_leader_id: target_node_id,
        })
    }

    async fn start_node(&self, node_id: u64) -> Result<NodeActionResponse, String> {
        pgvisor_core::log_highlight("START NODE");
        info!(node_id, "Executing start command on node");
        let (target, status) = self.find_target(node_id).await?;
        if status.status == "running" {
            return Err(format!("Node #{} is already running", node_id));
        }

        let start_url = format!("{}/control/start", target.control_url.trim_end_matches('/'));
        let resp = self
            .http_client
            .post(&start_url)
            .send()
            .await
            .map_err(|e| format!("Failed to send start request to {}: {}", start_url, e))?;

        if !resp.status().is_success() {
            let err_body = resp.text().await.unwrap_or_default();
            return Err(format!("Start failed on Node #{}: {}", node_id, err_body));
        }

        if let Some(audit) = self.audit_log.as_ref() {
            audit
                .append(
                    AuditEventKind::NodeUp,
                    Some(node_id),
                    Some(&target.pg_addr),
                    format!("Node #{} started from dashboard", node_id),
                    None,
                )
                .await;
        }

        Ok(NodeActionResponse {
            status: "ok".into(),
            message: format!("Node #{} started successfully", node_id),
            node_id,
            action: NodeLifecycleAction::Start,
        })
    }

    async fn stop_node(&self, node_id: u64) -> Result<NodeActionResponse, String> {
        pgvisor_core::log_highlight("STOP NODE");
        info!(node_id, "Executing stop command on node");
        let (target, status) = self.find_target(node_id).await?;
        if status.status == "stopped" {
            return Err(format!("Node #{} is already stopped", node_id));
        }

        let stop_url = format!("{}/control/stop", target.control_url.trim_end_matches('/'));
        let resp = self
            .http_client
            .post(&stop_url)
            .send()
            .await
            .map_err(|e| format!("Failed to send stop request to {}: {}", stop_url, e))?;

        if !resp.status().is_success() {
            let err_body = resp.text().await.unwrap_or_default();
            return Err(format!("Stop failed on Node #{}: {}", node_id, err_body));
        }

        // Drain pool connections so clients don't encounter dead TCP sockets
        self.pool.drain_all().await;

        if let Some(audit) = self.audit_log.as_ref() {
            audit
                .append(
                    AuditEventKind::NodeDown,
                    Some(node_id),
                    Some(&target.pg_addr),
                    format!("Node #{} stopped from dashboard", node_id),
                    None,
                )
                .await;
        }

        Ok(NodeActionResponse {
            status: "ok".into(),
            message: format!("Node #{} stopped cleanly", node_id),
            node_id,
            action: NodeLifecycleAction::Stop,
        })
    }

    async fn restart_node(&self, node_id: u64) -> Result<NodeActionResponse, String> {
        info!(node_id, "Executing restart command on node");
        let (target, _status) = self.find_target(node_id).await?;

        let restart_url = format!(
            "{}/control/restart",
            target.control_url.trim_end_matches('/')
        );
        let resp = self
            .http_client
            .post(&restart_url)
            .send()
            .await
            .map_err(|e| format!("Failed to send restart request to {}: {}", restart_url, e))?;

        if !resp.status().is_success() {
            let err_body = resp.text().await.unwrap_or_default();
            return Err(format!("Restart failed on Node #{}: {}", node_id, err_body));
        }

        // Drain pool connections so clients refresh connections after restart
        self.pool.drain_all().await;

        if let Some(audit) = self.audit_log.as_ref() {
            audit
                .append(
                    AuditEventKind::NodeUp,
                    Some(node_id),
                    Some(&target.pg_addr),
                    format!("Node #{} restarted from dashboard", node_id),
                    None,
                )
                .await;
        }

        Ok(NodeActionResponse {
            status: "ok".into(),
            message: format!("Node #{} restarted successfully", node_id),
            node_id,
            action: NodeLifecycleAction::Restart,
        })
    }
}
