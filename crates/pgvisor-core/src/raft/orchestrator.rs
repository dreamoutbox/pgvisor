use std::time::{Duration, Instant};
use tracing::{error, info, warn};

use crate::raft::types::NodeRole;

/// Quorum lease tracker used by the leader to ensure fencing before standby promotion.
#[derive(Debug, Clone)]
pub struct QuorumLease {
    lease_duration: Duration,
    last_quorum_ack: Instant,
}

impl QuorumLease {
    pub fn new(lease_duration: Duration) -> Self {
        Self {
            lease_duration,
            last_quorum_ack: Instant::now(),
        }
    }

    /// Renews the lease timestamp upon receiving heartbeats from a majority quorum.
    pub fn renew(&mut self, now: Instant) {
        self.last_quorum_ack = now;
    }

    /// Checks whether the leader's lease remains valid.
    pub fn is_valid(&self, now: Instant) -> bool {
        now.duration_since(self.last_quorum_ack) <= self.lease_duration
    }

    /// Duration remaining on the current lease.
    pub fn remaining_time(&self, now: Instant) -> Duration {
        let elapsed = now.duration_since(self.last_quorum_ack);
        if elapsed >= self.lease_duration {
            Duration::ZERO
        } else {
            self.lease_duration - elapsed
        }
    }
}

/// Action to execute on the underlying Postgres supervisor following consensus events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrchestratorAction {
    Noop,
    PromoteToLeader,
    DemoteToStandby { primary_node_id: u64 },
    FenceImmediately,
}

/// High-availability failover orchestrator for the local sidecar instance.
pub struct FailoverOrchestrator {
    node_id: u64,
    current_role: NodeRole,
    lease: QuorumLease,
}

impl FailoverOrchestrator {
    pub fn new(node_id: u64, lease_duration: Duration) -> Self {
        Self {
            node_id,
            current_role: NodeRole::Standby,
            lease: QuorumLease::new(lease_duration),
        }
    }

    pub fn current_role(&self) -> NodeRole {
        self.current_role
    }

    /// Handles a new leader election notification from OpenRaft.
    pub fn on_leader_elected(&mut self, leader_id: u64, now: Instant) -> OrchestratorAction {
        if leader_id == self.node_id {
            if self.current_role != NodeRole::Leader {
                info!(
                    node_id = self.node_id,
                    "Elected Raft Leader: initiating Postgres promotion"
                );
                self.current_role = NodeRole::Leader;
                self.lease.renew(now);
                return OrchestratorAction::PromoteToLeader;
            }
        } else {
            if self.current_role == NodeRole::Leader {
                warn!(
                    node_id = self.node_id,
                    new_leader = leader_id,
                    "Demoted from Leader: fencing immediately to prevent split-brain"
                );
                self.current_role = NodeRole::Fenced;
                return OrchestratorAction::FenceImmediately;
            } else {
                self.current_role = NodeRole::Standby;
                return OrchestratorAction::DemoteToStandby {
                    primary_node_id: leader_id,
                };
            }
        }
        OrchestratorAction::Noop
    }

    /// Periodic heartbeat check for the current leader. If quorum is lost, fences immediately.
    pub fn check_quorum_lease(&mut self, now: Instant) -> OrchestratorAction {
        if self.current_role == NodeRole::Leader && !self.lease.is_valid(now) {
            error!(
                node_id = self.node_id,
                "Quorum lease expired without heartbeat acks! Fencing Postgres immediately."
            );
            self.current_role = NodeRole::Fenced;
            return OrchestratorAction::FenceImmediately;
        }
        OrchestratorAction::Noop
    }

    /// Renews quorum lease when a majority of peer nodes acknowledge AppendEntries.
    pub fn on_quorum_acknowledged(&mut self, now: Instant) {
        if self.current_role == NodeRole::Leader {
            self.lease.renew(now);
        }
    }

    /// Recovers a fenced or restarted node into standby mode.
    pub fn recover_to_standby(&mut self, primary_node_id: u64) -> OrchestratorAction {
        info!(
            node_id = self.node_id,
            primary_node_id, "Recovering node into standby replica mode"
        );
        self.current_role = NodeRole::Standby;
        OrchestratorAction::DemoteToStandby { primary_node_id }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quorum_lease_expiry() {
        let mut lease = QuorumLease::new(Duration::from_millis(100));
        let t0 = Instant::now();

        assert!(lease.is_valid(t0));
        assert!(lease.is_valid(t0 + Duration::from_millis(50)));

        // Exceed lease duration
        assert!(!lease.is_valid(t0 + Duration::from_millis(150)));

        // Renew
        lease.renew(t0 + Duration::from_millis(150));
        assert!(lease.is_valid(t0 + Duration::from_millis(160)));
    }

    #[test]
    fn test_failover_election_and_fencing() {
        let mut orch = FailoverOrchestrator::new(1, Duration::from_millis(200));
        let t0 = Instant::now();

        // Standby becomes leader
        let act = orch.on_leader_elected(1, t0);
        assert_eq!(act, OrchestratorAction::PromoteToLeader);
        assert_eq!(orch.current_role(), NodeRole::Leader);

        // Acks keep lease alive
        orch.on_quorum_acknowledged(t0 + Duration::from_millis(100));
        let act = orch.check_quorum_lease(t0 + Duration::from_millis(150));
        assert_eq!(act, OrchestratorAction::Noop);

        // Network split: lease expires
        let act = orch.check_quorum_lease(t0 + Duration::from_millis(400));
        assert_eq!(act, OrchestratorAction::FenceImmediately);
        assert_eq!(orch.current_role(), NodeRole::Fenced);

        // Recovery
        let act = orch.recover_to_standby(2);
        assert_eq!(
            act,
            OrchestratorAction::DemoteToStandby { primary_node_id: 2 }
        );
        assert_eq!(orch.current_role(), NodeRole::Standby);
    }
}
