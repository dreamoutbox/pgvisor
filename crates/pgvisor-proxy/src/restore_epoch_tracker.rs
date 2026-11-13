use tracing::info;

use crate::pool::ConnectionPool;

/// Tracks the cluster leader's monotonic `restore_epoch` and drains the proxy pool when it advances.
#[derive(Debug, Default)]
pub struct RestoreEpochTracker {
    last_leader_epoch: Option<u64>,
}

impl RestoreEpochTracker {
    /// Creates an empty tracker awaiting the first leader status poll.
    pub fn new() -> Self {
        Self::default()
    }

    /// Evaluates a newly polled leader `restore_epoch` and returns `true` if `pool.drain_all()` should run.
    pub fn observe(&mut self, observed_epoch: Option<u64>) -> bool {
        let Some(epoch) = observed_epoch else {
            return false;
        };

        match self.last_leader_epoch {
            None => {
                self.last_leader_epoch = Some(epoch);
                false
            }
            Some(prev) if epoch > prev => {
                self.last_leader_epoch = Some(epoch);
                true
            }
            Some(_) => false,
        }
    }

    /// Observes `observed_epoch` and drains all pooled backend connections if the epoch increased.
    pub async fn observe_and_drain(&mut self, pool: &ConnectionPool, observed_epoch: Option<u64>) {
        if self.observe(observed_epoch) {
            info!(
                restore_epoch = ?self.last_leader_epoch,
                "Leader restore_epoch increased; draining proxy connection pool to realign timelines"
            );
            pool.drain_all().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    use axum::routing::get;
    use axum::{Json, Router};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;
    use crate::pool::BackendRole;

    #[tokio::test]
    async fn test_epoch_triggered_drain_in_proxy_monitor() {
        let epoch = Arc::new(AtomicU64::new(0));
        let epoch_route = Arc::clone(&epoch);
        let control_app = Router::new().route(
            "/control/status",
            get(move || {
                let e = Arc::clone(&epoch_route);
                async move {
                    Json(serde_json::json!({
                        "node_id": 1,
                        "role": "leader",
                        "status": "running",
                        "restore_epoch": e.load(Ordering::SeqCst),
                    }))
                }
            }),
        );

        let control_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let control_addr = control_listener.local_addr().unwrap();
        let control_task = tokio::spawn(async move {
            let _ = axum::serve(control_listener, control_app).await;
        });

        let pg_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let pg_addr = pg_listener.local_addr().unwrap().to_string();
        let pg_task = tokio::spawn(async move {
            let mut held_sockets = Vec::new();
            while let Ok((mut sock, _)) = pg_listener.accept().await {
                let mut buf = [0u8; 256];
                let _ = sock.read(&mut buf).await;
                // Send AuthenticationOk + ReadyForQuery('I')
                let startup_reply = [
                    b'R', 0, 0, 0, 8, 0, 0, 0, 0, // AuthenticationOk
                    b'Z', 0, 0, 0, 5, b'I', // ReadyForQuery
                ];
                let _ = sock.write_all(&startup_reply).await;
                held_sockets.push(sock);
            }
        });

        let pool = ConnectionPool::new(4);
        pool.update_topology(Some(pg_addr), vec![]).await;
        let conn = pool.acquire(BackendRole::Leader).await.unwrap();
        pool.release(conn).await;
        assert_eq!(pool.idle_count().await, (1, 0));

        let client = reqwest::Client::new();
        let status_url = format!("http://{control_addr}/control/status");
        let mut tracker = RestoreEpochTracker::new();

        // First poll records epoch 0 and keeps warm idle connections intact
        let body: serde_json::Value = client
            .get(&status_url)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        tracker
            .observe_and_drain(&pool, body["restore_epoch"].as_u64())
            .await;
        assert_eq!(pool.idle_count().await, (1, 0));

        // Bumping leader restore_epoch triggers pool.drain_all()
        epoch.store(1, Ordering::SeqCst);
        let body2: serde_json::Value = client
            .get(&status_url)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        tracker
            .observe_and_drain(&pool, body2["restore_epoch"].as_u64())
            .await;
        assert_eq!(pool.idle_count().await, (0, 0));

        // Subsequent polls at the same epoch do not re-drain newly pooled connections
        let conn2 = pool.acquire(BackendRole::Leader).await.unwrap();
        pool.release(conn2).await;
        assert_eq!(pool.idle_count().await, (1, 0));
        tracker.observe_and_drain(&pool, Some(1)).await;
        assert_eq!(pool.idle_count().await, (1, 0));

        control_task.abort();
        pg_task.abort();
    }
}
