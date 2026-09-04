pub mod executor;
pub mod pool;
pub mod session;

use std::env;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use executor::ProxySqlExecutor;
use pgvisor_dashboard::create_router;
use pgvisor_dashboard::handlers::DashboardState;
use pgvisor_dashboard::models::{NodeHealthState, NodeRole, NodeSummary};
use pool::ConnectionPool;
use session::ClientSession;
use tokio::net::TcpListener;
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

    // Spawn embedded dashboard on PGVISOR_DASHBOARD_LISTEN (default 0.0.0.0:8080)
    let dashboard_listen =
        env::var("PGVISOR_DASHBOARD_LISTEN").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    if let Ok(dash_addr) = dashboard_listen.parse::<SocketAddr>() {
        let cluster_id =
            env::var("PGVISOR_CLUSTER_ID").unwrap_or_else(|_| "pgvisor-cluster".to_string());
        let admin_token = env::var("PGVISOR_ADMIN_TOKEN").ok();
        let mut dash_state_inner = DashboardState::new(&cluster_id, admin_token);
        dash_state_inner.sql_executor = Arc::new(ProxySqlExecutor::new(pool.clone()));
        let dash_state = Arc::new(dash_state_inner);

        // Populate dashboard node topology from configured addresses
        {
            let mut overview = dash_state.overview.write().await;
            let mut nodes = Vec::new();
            let mut node_id = 1u64;

            if let Some(ref l_addr) = leader_addr {
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
