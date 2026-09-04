---
id: "004"
title: "Raft State Machine and Failover Orchestration"
type: "grilling"
status: "closed"
assignee: "antigravity"
blocked_by: []
---

## Question

What are the exact OpenRaft state machine command types, leader election triggers, heartbeat timeouts, quorum lease timers, and step-by-step fencing / promotion execution sequences during node failure, departure, and recovery?

## Resolution

Implemented Raft failover orchestration and quorum lease management in `crates/pgvisor-core/src/raft/orchestrator.rs`:
1. **Timing & Quorum Lease Parameters**:
   - Heartbeat interval: `500ms`.
   - Election timeout: `1500ms .. 3000ms` (randomized).
   - Quorum lease duration: `1200ms`. Because lease duration (1200ms) is strictly less than minimum election timeout (1500ms), an isolated leader is guaranteed to expire its lease and halt Postgres before any partitioned standby can win election and promote.
2. **Quorum Lease Tracker (`QuorumLease`)**:
   - Leader tracks instant of last majority heartbeat acknowledgment.
   - If heartbeats lapse beyond 1200ms, triggers `FenceImmediately`.
3. **Failover Orchestrator (`FailoverOrchestrator`)**:
   - `on_leader_elected`: Standby transitions to Leader, initiates `PromoteToLeader` (`pg_ctl promote`), and renews lease. Demoted former leader triggers `FenceImmediately`.
   - `check_quorum_lease`: Leader checks lease validity; triggers emergency fencing on network split.
   - `recover_to_standby`: Reattaches recovered or restarted nodes back to the cluster as replication standbys.
4. **Unit Tests**:
   - Verified lease validity, expiry, and renewal.
   - Verified state machine transitions: election promotion, split-brain fence triggering, and standby recovery.
