pub mod backup;
pub mod executor;
pub mod pool;
pub mod session;

use std::env;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use backup::ProxyBackupService;
use executor::ProxySqlExecutor;
use pgvisor_core::backup::{BackupManager, BackupScheduleConfig};
use pgvisor_dashboard::create_router;
use pgvisor_dashboard::handlers::DashboardState;
use pgvisor_dashboard::models::{NodeHealthState, NodeRole, NodeSummary};
use pool::ConnectionPool;
use session::ClientSession;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tracing::{error, info};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
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

    let pool = ConnectionPool::new(10);
    pool.update_topology(leader_addr.clone(), standby_addrs.clone())
        .await;

    let leader_ref = Arc::new(RwLock::new(leader_addr.clone()));
    let standby_ref = Arc::new(RwLock::new(standby_addrs.clone()));

    let control_port = env::var("PGVISOR_CONTROL_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);

    // Build cluster targets for dynamic topology monitoring
    struct NodeTarget {
        pg_addr: String,
        control_url: String,
    }

    let mut all_pg_addrs = Vec::new();
    if let Some(l) = leader_addr.as_ref() {
        all_pg_addrs.push(l.clone());
    }
    for s in &standby_addrs {
        all_pg_addrs.push(s.clone());
    }

    let targets: Vec<NodeTarget> = all_pg_addrs
        .into_iter()
        .map(|pg_addr| {
            let host = pg_addr.split(':').next().unwrap_or(&pg_addr);
            let control_url = format!("http://{}:{}", host, control_port);
            NodeTarget {
                pg_addr,
                control_url,
            }
        })
        .collect();

    #[derive(serde::Deserialize, Debug)]
    struct ControlStatus {
        node_id: u64,
        role: String,
        status: String,
    }

    let mut dash_state_opt: Option<Arc<DashboardState>> = None;

    // Spawn embedded dashboard on PGVISOR_DASHBOARD_LISTEN (default 0.0.0.0:8080)
    let dashboard_listen =
        env::var("PGVISOR_DASHBOARD_LISTEN").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    if let Ok(dash_addr) = dashboard_listen.parse::<SocketAddr>() {
        let cluster_id =
            env::var("PGVISOR_CLUSTER_ID").unwrap_or_else(|_| "pgvisor-cluster".to_string());
        let admin_token = env::var("PGVISOR_ADMIN_TOKEN").ok();
        let mut dash_state_inner = DashboardState::new(&cluster_id, admin_token);
        dash_state_inner.sql_executor = Arc::new(ProxySqlExecutor::new(pool.clone()));

        // Initialize OpenDAL backup manager for MinIO/S3
        let s3_endpoint =
            env::var("S3_ENDPOINT").unwrap_or_else(|_| "http://minio:9000".to_string());
        let s3_bucket = env::var("S3_BUCKET").unwrap_or_else(|_| "pgvisor-backups".to_string());
        let s3_access_key = env::var("S3_ACCESS_KEY").unwrap_or_else(|_| "minioadmin".to_string());
        let s3_secret_key = env::var("S3_SECRET_KEY").unwrap_or_else(|_| "minioadmin".to_string());

        let backup_config = BackupScheduleConfig {
            minio_endpoint: s3_endpoint.clone(),
            minio_bucket: s3_bucket.clone(),
            access_key: s3_access_key,
            secret_key: s3_secret_key,
            ..Default::default()
        };

        let backup_service: Arc<dyn pgvisor_dashboard::handlers::BackupService> =
            match backup_config.build_operator() {
                Ok(operator) => {
                    let bm = Arc::new(BackupManager::new(&cluster_id, operator));
                    Arc::new(ProxyBackupService::new(
                        bm,
                        leader_ref.clone(),
                        standby_ref.clone(),
                        Some(pool.clone()),
                        s3_endpoint,
                        s3_bucket,
                        backup_config.retention_days,
                        control_port,
                    ))
                }
                Err(e) => {
                    error!(
                        ?e,
                        "Failed to initialize OpenDAL S3 operator, using fallback"
                    );
                    Arc::new(pgvisor_dashboard::handlers::StandaloneBackupService::new())
                }
            };
        dash_state_inner.backup_service = backup_service;

        let dash_state = Arc::new(dash_state_inner);
        dash_state_opt = Some(dash_state.clone());

        // Populate initial dashboard node topology from configured addresses
        {
            let mut overview = dash_state.overview.write().await;
            let mut nodes = Vec::new();
            let mut node_id = 1u64;

            if let Some(l_addr) = leader_addr.as_ref() {
                nodes.push(NodeSummary {
                    node_id,
                    address: l_addr.clone(),
                    role: NodeRole::Leader,
                    state: NodeHealthState::Healthy,
                    pg_version: "16.3".into(),
                    replication_lag_bytes: 0,
                    uptime_secs: 100,
                    is_local: false,
                });
                node_id += 1;
            }

            for s_addr in &standby_addrs {
                nodes.push(NodeSummary {
                    node_id,
                    address: s_addr.clone(),
                    role: NodeRole::Standby,
                    state: NodeHealthState::Healthy,
                    pg_version: "16.3".into(),
                    replication_lag_bytes: 64,
                    uptime_secs: 100,
                    is_local: false,
                });
                node_id += 1;
            }

            overview.total_nodes = nodes.len();
            overview.healthy_nodes = nodes.len();
            overview.leader_address = leader_addr.clone();
            overview.nodes = nodes;
        }

        let dash_app = create_router(dash_state);
        tokio::spawn(async move {
            if let Ok(listener) = tokio::net::TcpListener::bind(dash_addr).await {
                info!(addr = %dash_addr, "PgVisor embedded dashboard listening");
                let _ = axum::serve(listener, dash_app).await;
            }
        });
    }

    // Background dynamic topology discovery loop
    let pool_for_monitor = pool.clone();
    let leader_ref_monitor = leader_ref.clone();
    let standby_ref_monitor = standby_ref.clone();
    let dash_state_for_monitor = dash_state_opt.clone();

    tokio::spawn(async move {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(400))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));

        loop {
            interval.tick().await;

            let mut discovered_leader: Option<String> = None;
            let mut discovered_standbys: Vec<String> = Vec::new();
            let mut node_summaries = Vec::new();
            let mut healthy_count = 0;

            for target in &targets {
                let url = format!("{}/control/status", target.control_url.trim_end_matches('/'));
                match client.get(&url).send().await {
                    Ok(resp) => {
                        if let Ok(st) = resp.json::<ControlStatus>().await {
                            let is_healthy = st.status == "running";
                            let state = match st.status.as_str() {
                                "running" => {
                                    healthy_count += 1;
                                    NodeHealthState::Healthy
                                }
                                "fenced" => NodeHealthState::Fenced,
                                _ => NodeHealthState::Offline,
                            };

                            let role = if st.role == "leader" {
                                if is_healthy {
                                    discovered_leader = Some(target.pg_addr.clone());
                                }
                                NodeRole::Leader
                            } else {
                                if is_healthy {
                                    discovered_standbys.push(target.pg_addr.clone());
                                }
                                NodeRole::Standby
                            };

                            node_summaries.push(NodeSummary {
                                node_id: st.node_id,
                                address: target.pg_addr.clone(),
                                role,
                                state,
                                pg_version: "16.3".into(),
                                replication_lag_bytes: 0,
                                uptime_secs: 100,
                                is_local: false,
                            });
                        } else {
                            node_summaries.push(NodeSummary {
                                node_id: 0,
                                address: target.pg_addr.clone(),
                                role: NodeRole::Standby,
                                state: NodeHealthState::Offline,
                                pg_version: "16.3".into(),
                                replication_lag_bytes: 0,
                                uptime_secs: 0,
                                is_local: false,
                            });
                        }
                    }
                    Err(_) => {
                        node_summaries.push(NodeSummary {
                            node_id: 0,
                            address: target.pg_addr.clone(),
                            role: NodeRole::Standby,
                            state: NodeHealthState::Offline,
                            pg_version: "16.3".into(),
                            replication_lag_bytes: 0,
                            uptime_secs: 0,
                            is_local: false,
                        });
                    }
                }
            }

            // Check if leader changed
            let current_leader = {
                let r = leader_ref_monitor.read().await;
                r.clone()
            };

            if discovered_leader.is_some() && (discovered_leader != current_leader) {
                info!(
                    old_leader = ?current_leader,
                    new_leader = ?discovered_leader,
                    standbys = ?discovered_standbys,
                    "Active leader change detected! Updating connection pool topology"
                );

                {
                    let mut l = leader_ref_monitor.write().await;
                    *l = discovered_leader.clone();
                }
                {
                    let mut s = standby_ref_monitor.write().await;
                    *s = discovered_standbys.clone();
                }

                pool_for_monitor
                    .update_topology(discovered_leader.clone(), discovered_standbys.clone())
                    .await;
            }

            // Update dashboard overview
            if let Some(dash) = dash_state_for_monitor.as_ref() {
                let mut overview = dash.overview.write().await;
                overview.total_nodes = targets.len();
                overview.healthy_nodes = healthy_count;
                overview.leader_address = discovered_leader;
                overview.nodes = node_summaries;
            }
        }
    });

    let listener = TcpListener::bind(&listen_addr).await?;
    info!(%listen_addr, "Listening for PostgreSQL client connections");

    loop {
        let (socket, peer_addr) = listener.accept().await?;
        info!(%peer_addr, "Accepted incoming PostgreSQL client connection");

        let pool_clone = pool.clone();
        tokio::spawn(async move {
            let mut session = ClientSession::new(socket, pool_clone);
            if let Err(err) = session.run().await {
                error!(%peer_addr, %err, "Session terminated with error");
            }
        });
    }
}
