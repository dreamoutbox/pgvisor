use std::sync::Arc;
use std::time::Duration;

use pgvisor_core::{extract_node_name, format_become_leader_highlight, log_highlight};
use tracing::{error, info, warn};

use crate::control::{SidecarState, StatusResponse};
use crate::supervisor::ProcessStatus;

/// Spawns the background heartbeat & auto-failover election monitor.
pub fn spawn_election_monitor(monitor_state: SidecarState, peers: Arc<Vec<String>>) {
    if peers.is_empty() {
        return;
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(800))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    tokio::spawn(async move {
        let mut missed_heartbeats = 0u32;
        let mut interval = tokio::time::interval(Duration::from_millis(500));

        loop {
            interval.tick().await;

            let local_status = monitor_state.supervisor.status().await;
            if local_status == ProcessStatus::Restoring {
                // Node is actively restoring or re-syncing; pause auto-failover actions
                missed_heartbeats = 0;
                continue;
            }

            let local_role = monitor_state.role.read().await.clone();
            if local_status == ProcessStatus::Stopped && local_role != "fenced" {
                // Node is intentionally stopped; pause auto-failover actions
                missed_heartbeats = 0;
                continue;
            }

            if local_role == "fenced" {
                // Check if an active leader is operating and available for auto-rejoin
                let mut active_leader: Option<(u64, String)> = None;

                for peer in peers.iter() {
                    let self_tag = format!("node{}", monitor_state.node_id);
                    if peer.contains(&self_tag) {
                        continue;
                    }
                    let url = format!("{}/control/status", peer.trim_end_matches('/'));
                    if let Ok(resp) = client.get(&url).send().await {
                        if let Ok(st) = resp.json::<StatusResponse>().await {
                            if st.role == "leader" && st.status == "running" {
                                let conninfo = format!(
                                    "host=pgvisor-node{} port=5432 user=postgres application_name=pgvisor-node{}",
                                    st.node_id, monitor_state.node_id
                                );
                                active_leader = Some((st.node_id, conninfo));
                                break;
                            }
                        }
                    }
                }

                if let Some((leader_id, conninfo)) = active_leader {
                    info!(
                        node_id = monitor_state.node_id,
                        leader_id,
                        "Fenced node detected active cluster leader. Initiating auto-rejoin as standby replica."
                    );

                    let mut standby_config = monitor_state.config.read().await.clone();
                    standby_config.primary_conninfo = Some(conninfo.clone());

                    match monitor_state
                        .supervisor
                        .resync_from_primary(&conninfo, &standby_config)
                        .await
                    {
                        Ok(()) => {
                            info!(
                                node_id = monitor_state.node_id,
                                leader_id, "Successfully auto-rejoined cluster as standby replica."
                            );
                            {
                                let mut r = monitor_state.role.write().await;
                                *r = "standby".to_string();
                                let mut cfg = monitor_state.config.write().await;
                                cfg.primary_conninfo = Some(conninfo.clone());
                            }
                            monitor_state
                                .record_event(
                                    "node_joined",
                                    format!(
                                        "Node {} auto-rejoined cluster as standby under leader {}",
                                        monitor_state.node_id, leader_id
                                    ),
                                )
                                .await;
                            missed_heartbeats = 0;
                        }
                        Err(e) => {
                            error!(
                                node_id = monitor_state.node_id,
                                leader_id,
                                ?e,
                                "Failed to auto-rejoin as standby; will retry on next cycle"
                            );
                            tokio::time::sleep(Duration::from_secs(2)).await;
                        }
                    }
                }
                continue;
            }

            if local_role == "leader" {
                // Split-brain guard: check if another peer is already operating as active leader
                for peer in peers.iter() {
                    let self_tag = format!("node{}", monitor_state.node_id);
                    if peer.contains(&self_tag) {
                        continue;
                    }
                    let url = format!("{}/control/status", peer.trim_end_matches('/'));
                    if let Ok(resp) = client.get(&url).send().await {
                        if let Ok(st) = resp.json::<StatusResponse>().await {
                            if st.role == "leader" && st.status == "running" {
                                warn!(
                                    node_id = monitor_state.node_id,
                                    peer_leader = st.node_id,
                                    "Split-brain detected: another node is already active leader! Fencing local instance to prevent data corruption."
                                );
                                let _ = monitor_state.supervisor.fence().await;
                                {
                                    let mut r = monitor_state.role.write().await;
                                    *r = "fenced".to_string();
                                }
                                monitor_state
                                    .record_event(
                                        "election_result",
                                        format!(
                                            "Node {} fenced due to split-brain leader detection",
                                            monitor_state.node_id
                                        ),
                                    )
                                    .await;
                                break;
                            }
                        }
                    }
                }
                continue;
            }

            // Standby mode: probe peers for active leader
            let mut leader_found = false;
            let mut alive_nodes: Vec<u64> = vec![monitor_state.node_id];

            for peer in peers.iter() {
                let url = format!("{}/control/status", peer.trim_end_matches('/'));
                if let Ok(resp) = client.get(&url).send().await {
                    if let Ok(st) = resp.json::<StatusResponse>().await {
                        if st.status == "running" || st.status == "restoring" {
                            alive_nodes.push(st.node_id);
                            if st.role == "leader" {
                                leader_found = true;
                            }
                        } else if st.role == "leader" {
                            alive_nodes.push(st.node_id);
                            leader_found = true;
                        }
                    }
                }
            }

            alive_nodes.sort_unstable();
            alive_nodes.dedup();

            if leader_found {
                missed_heartbeats = 0;
            } else {
                missed_heartbeats += 1;
                info!(
                    node_id = monitor_state.node_id,
                    missed_heartbeats,
                    alive_count = alive_nodes.len(),
                    "No active leader detected"
                );

                // Election timeout: 5 consecutive misses (2500ms) matches Ticket 004
                if missed_heartbeats >= 5 {
                    let quorum = (peers.len() / 2) + 1;
                    if alive_nodes.len() >= quorum {
                        if let Some(&winner_id) = alive_nodes.first() {
                            if winner_id == monitor_state.node_id {
                                let old_conninfo = monitor_state
                                    .config
                                    .read()
                                    .await
                                    .primary_conninfo
                                    .clone()
                                    .unwrap_or_default();
                                let old_node = extract_node_name(&old_conninfo);
                                let old_leader = if old_node == "unknown"
                                    || old_node.is_empty()
                                    || old_node == format!("node{}", monitor_state.node_id)
                                {
                                    "node1".to_string()
                                } else {
                                    old_node
                                };
                                let my_node = format!("node{}", monitor_state.node_id);
                                log_highlight(&format_become_leader_highlight(
                                    &old_leader,
                                    &my_node,
                                ));

                                info!(
                                    node_id = monitor_state.node_id,
                                    "Quorum achieved and candidate ID matches: PROMOTING TO LEADER"
                                );
                                if let Err(e) = monitor_state.supervisor.promote().await {
                                    error!(?e, "Failed to promote PostgreSQL to leader");
                                } else {
                                    {
                                        let mut r = monitor_state.role.write().await;
                                        *r = "leader".to_string();
                                        let mut cfg = monitor_state.config.write().await;
                                        cfg.primary_conninfo = None;
                                    }
                                    monitor_state
                                        .record_event(
                                            "election_result",
                                            format!(
                                                "Node {} auto-promoted to leader after quorum election",
                                                monitor_state.node_id
                                            ),
                                        )
                                        .await;
                                    missed_heartbeats = 0;

                                    // Broadcast repoint to peer standbys (excluding self)
                                    let my_conninfo = format!(
                                        "host=pgvisor-node{} port=5432 user=postgres",
                                        monitor_state.node_id
                                    );
                                    let self_host = format!("node{}:", monitor_state.node_id);
                                    let self_host2 = format!("node{}", monitor_state.node_id);
                                    for peer in peers.iter() {
                                        if peer.contains(&self_host) || peer.ends_with(&self_host2)
                                        {
                                            continue;
                                        }
                                        let repoint_url = format!(
                                            "{}/control/repoint",
                                            peer.trim_end_matches('/')
                                        );
                                        let payload = serde_json::json!({
                                            "primary_conninfo": my_conninfo
                                        });
                                        let _ =
                                            client.post(&repoint_url).json(&payload).send().await;
                                    }
                                }
                            } else {
                                info!(
                                    node_id = monitor_state.node_id,
                                    winner_id, "Waiting for peer candidate to promote"
                                );
                            }
                        }
                    } else {
                        warn!(
                            node_id = monitor_state.node_id,
                            alive = alive_nodes.len(),
                            required = quorum,
                            "Quorum lost; cannot elect new leader"
                        );
                    }
                }
            }
        }
    });
}
