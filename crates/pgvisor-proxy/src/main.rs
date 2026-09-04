pub mod pool;
pub mod session;

use std::env;
use anyhow::Result;
use pool::ConnectionPool;
use session::ClientSession;
use tokio::net::TcpListener;
use tracing::{error, info};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let listen_addr = env::var("PGVISOR_PROXY_LISTEN").unwrap_or_else(|_| "0.0.0.0:5432".to_string());
    let leader_addr = env::var("PGVISOR_LEADER_ADDR").ok();

    info!(%listen_addr, ?leader_addr, "pgvisor-proxy service starting up");

    let pool = ConnectionPool::new(10);
    if let Some(leader) = leader_addr {
        pool.update_topology(Some(leader), Vec::new()).await;
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
