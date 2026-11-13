pub mod node_target;
pub mod pool;
pub mod restore_epoch_tracker;
pub mod session;
pub mod tls;

use std::env;
use std::sync::Arc;

use anyhow::Result;
use node_target::NodeTarget;
use pgvisor_core::pg_client::execute_simple_query_on_stream;
use pool::{BackendRole, ConnectionPool, FailoverConfig};
use restore_epoch_tracker::RestoreEpochTracker;
use session::ClientSession;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

const REPLICATION_DISCOVERY_SQL: &str =
    "SELECT client_addr::text, application_name FROM pg_stat_replication;";

#[derive(serde::Deserialize, Debug)]
struct ControlStatus {
    role: String,
    status: String,
    #[serde(default)]
    restore_epoch: u64,
}

/// Queries the active leader for connected streaming-replication replicas.
async fn query_replication_peers(
    pool: &ConnectionPool,
    failover_config: &FailoverConfig,
) -> Option<Vec<Vec<String>>> {
    let mut backend = pool
        .acquire_with_retry(BackendRole::Leader, failover_config, None, None)
        .await
        .ok()?;
    let result =
        execute_simple_query_on_stream(&mut backend.stream, REPLICATION_DISCOVERY_SQL, 50).await;
    match result {
        Ok(query_result) => {
            pool.release(backend).await;
            Some(query_result.rows)
        }
        Err(_) => None,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_ansi(false)
        .init();

    let listen_addr =
        env::var("PGVISOR_PROXY_LISTEN").unwrap_or_else(|_| "0.0.0.0:5432".to_string());
    let leader_addr = env::var("PGVISOR_LEADER_ADDR").ok();
    let standby_addrs_str = env::var("PGVISOR_STANDBY_ADDRS").unwrap_or_default();
    let standby_addrs: Vec<String> = standby_addrs_str
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    info!(%listen_addr, ?leader_addr, ?standby_addrs, "pgvisor-proxy service starting up");

    let tls_enabled = env::var("PGVISOR_TLS_ENABLED")
        .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
        .unwrap_or(false);

    let tls_required = env::var("PGVISOR_TLS_REQUIRED")
        .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
        .unwrap_or(false);

    let backend_tls = env::var("PGVISOR_BACKEND_TLS")
        .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
        .unwrap_or(tls_enabled);

    let tls_acceptor = if tls_enabled {
        let cert_dir = env::var("PGVISOR_TLS_CERT_DIR")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                if std::path::Path::new("/var/lib/postgresql").exists() {
                    std::path::PathBuf::from("/var/lib/postgresql/tls")
                } else {
                    std::path::PathBuf::from("./tls")
                }
            });
        let custom_cert = env::var("PGVISOR_TLS_CERT_FILE")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(std::path::PathBuf::from);
        let custom_key = env::var("PGVISOR_TLS_KEY_FILE")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(std::path::PathBuf::from);

        let (cert_p, key_p) = tls::load_or_generate_proxy_certs(cert_dir, custom_cert, custom_key)?;
        let acceptor = tls::build_client_acceptor(&cert_p, &key_p)?;
        info!(cert = ?cert_p, key = ?key_p, tls_required, "pgvisor-proxy TLS client listener initialized");
        Some(Arc::new(acceptor))
    } else {
        None
    };

    let mut pool = ConnectionPool::new(10);
    if backend_tls {
        let connector = tls::build_backend_connector()?;
        info!("pgvisor-proxy backend connection TLS enabled (sslmode=require)");
        pool = pool.with_tls_connector(Arc::new(connector));
    }
    pool.update_topology(leader_addr.clone(), standby_addrs.clone())
        .await;

    let leader_ref = Arc::new(RwLock::new(leader_addr.clone()));
    let standby_ref = Arc::new(RwLock::new(standby_addrs.clone()));

    let control_port = env::var("PGVISOR_CONTROL_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);

    let mut all_pg_addrs = Vec::new();
    if let Some(l) = leader_addr.as_ref() {
        all_pg_addrs.push(l.clone());
    }
    for s in &standby_addrs {
        all_pg_addrs.push(s.clone());
    }

    let targets = Arc::new(RwLock::new(
        all_pg_addrs
            .into_iter()
            .map(|pg_addr| {
                let host = pg_addr.split(':').next().unwrap_or(&pg_addr);
                let control_url = format!("http://{}:{}", host, control_port);
                NodeTarget {
                    pg_addr,
                    control_url,
                    is_dynamic: false,
                    consecutive_failures: 0,
                }
            })
            .collect::<Vec<_>>(),
    ));

    let cluster_secret = pgvisor_core::auth::cluster_secret_from_env();
    if cluster_secret.is_none() {
        warn!("PGVISOR_CLUSTER_SECRET is not configured; proxy-to-sidecar communication is unauthenticated");
    } else {
        info!("Cluster authentication enabled for proxy-to-sidecar communication");
    }

    // Background dynamic topology discovery loop
    let pool_for_monitor = pool.clone();
    let leader_ref_monitor = leader_ref.clone();
    let standby_ref_monitor = standby_ref.clone();
    let targets_monitor = targets.clone();
    let cluster_secret_monitor = cluster_secret.clone();

    tokio::spawn(async move {
        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(secret) = cluster_secret_monitor.as_deref() {
            if let Ok(val) = reqwest::header::HeaderValue::from_str(
                &pgvisor_core::auth::make_auth_header_value(secret),
            ) {
                headers.insert(reqwest::header::AUTHORIZATION, val);
            }
        }

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(800))
            .default_headers(headers)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        let discovery_failover = FailoverConfig {
            failover_timeout: std::time::Duration::from_millis(500),
            retry_interval: std::time::Duration::from_millis(100),
        };
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
        let mut restore_epoch_tracker = RestoreEpochTracker::new();

        loop {
            interval.tick().await;
            let mut discovered_leader_restore_epoch: Option<u64> = None;

            // Query active leader's pg_stat_replication to discover dynamic replicas
            if let Some(rows) = query_replication_peers(&pool_for_monitor, &discovery_failover).await {
                for row in rows {
                    if row.len() >= 2 {
                        let client_addr = &row[0];
                        let app_name = &row[1];
                        let host = if app_name.starts_with("pgvisor-node") {
                            app_name.clone()
                        } else if !client_addr.is_empty() && client_addr != "NULL" {
                            client_addr.clone()
                        } else {
                            continue;
                        };

                        let pg_addr = format!("{host}:5432");
                        let control_url = format!("http://{host}:{control_port}");

                        let mut t_lock = targets_monitor.write().await;
                        if !t_lock
                            .iter()
                            .any(|t| t.pg_addr == pg_addr || t.control_url == control_url)
                        {
                            info!(%pg_addr, %control_url, "Dynamically discovered new cluster replica from leader");
                            t_lock.push(NodeTarget {
                                pg_addr,
                                control_url,
                                is_dynamic: true,
                                consecutive_failures: 0,
                            });
                        }
                    }
                }
            }

            let mut discovered_leader: Option<String> = None;
            let mut discovered_standbys: Vec<String> = Vec::new();

            let targets_to_poll = {
                let r = targets_monitor.read().await;
                r.clone()
            };

            for target in &targets_to_poll {
                let url = format!(
                    "{}/control/status",
                    target.control_url.trim_end_matches('/')
                );
                let mut healthy_poll = false;
                if let Ok(resp) = client.get(&url).send().await {
                    if let Ok(st) = resp.json::<ControlStatus>().await {
                        healthy_poll = true;
                        let is_healthy = st.status == "running";

                        if st.role == "leader" {
                            if is_healthy || st.status == "restoring" {
                                discovered_leader = Some(target.pg_addr.clone());
                            }
                            discovered_leader_restore_epoch = Some(
                                discovered_leader_restore_epoch
                                    .map_or(st.restore_epoch, |prev| prev.max(st.restore_epoch)),
                            );
                        } else if is_healthy {
                            discovered_standbys.push(target.pg_addr.clone());
                        }

                        if target.is_dynamic {
                            let mut t_lock = targets_monitor.write().await;
                            if let Some(t) = t_lock.iter_mut().find(|t| t.pg_addr == target.pg_addr)
                            {
                                t.consecutive_failures = 0;
                            }
                        }
                    }
                }
                if !healthy_poll && target.is_dynamic {
                    let mut t_lock = targets_monitor.write().await;
                    if let Some(t) = t_lock.iter_mut().find(|t| t.pg_addr == target.pg_addr) {
                        t.consecutive_failures += 1;
                    }
                }
            }

            // Prune dynamic targets with consecutive failures >= 10
            {
                let mut t_lock = targets_monitor.write().await;
                t_lock.retain(|t| !(t.is_dynamic && t.consecutive_failures >= 10));
            }

            let current_leader = {
                let r = leader_ref_monitor.read().await;
                r.clone()
            };
            let current_standbys = {
                let r = standby_ref_monitor.read().await;
                r.clone()
            };

            let leader_changed = discovered_leader != current_leader;
            let standbys_changed = discovered_standbys != current_standbys;

            if leader_changed || standbys_changed {
                info!(
                    leader_changed,
                    standbys_changed,
                    old_leader = ?current_leader,
                    new_leader = ?discovered_leader,
                    standbys = ?discovered_standbys,
                    "Dynamic cluster topology change detected: updating proxy connection pool"
                );

                if leader_changed {
                    if let Some(ref new_l) = discovered_leader {
                        let old_node = current_leader
                            .as_deref()
                            .map(pgvisor_core::extract_node_name)
                            .unwrap_or_else(|| "node1".to_string());
                        let new_node = pgvisor_core::extract_node_name(new_l);
                        if old_node != new_node {
                            pgvisor_core::log_highlight(
                                &pgvisor_core::format_leader_down_highlight(&old_node, &new_node),
                            );
                        }
                    }
                    let mut l = leader_ref_monitor.write().await;
                    *l = discovered_leader.clone();
                }
                if standbys_changed {
                    let mut s = standby_ref_monitor.write().await;
                    *s = discovered_standbys.clone();
                }

                pool_for_monitor
                    .update_topology(discovered_leader, discovered_standbys)
                    .await;
            }

            // Drain idle connections if the sidecar leader bumped its restore_epoch.
            restore_epoch_tracker
                .observe_and_drain(&pool_for_monitor, discovered_leader_restore_epoch)
                .await;
        }
    });

    let listener = TcpListener::bind(&listen_addr).await?;
    info!(%listen_addr, "Listening for PostgreSQL client connections");

    let session_tls_acceptor = tls_acceptor.clone();
    loop {
        let (socket, peer_addr) = listener.accept().await?;
        info!(%peer_addr, "Accepted incoming PostgreSQL client connection");

        let pool_clone = pool.clone();
        let acceptor_clone = session_tls_acceptor.clone();
        tokio::spawn(async move {
            let mut session = ClientSession::new(socket, pool_clone);
            if let Some(acceptor) = acceptor_clone {
                session = session.with_tls_acceptor(acceptor);
            }
            if tls_required {
                session = session.with_tls_required(true);
            }
            if let Err(err) = session.run().await {
                error!(%peer_addr, %err, "Session terminated with error");
            }
        });
    }
}
