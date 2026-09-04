---
id: "000-fencing-and-split-brain-prevention"
title: "Fencing & Split-Brain Prevention"
type: "grilling"
status: "closed"
assignee: "antigravity"
blocked_by: []
---

## Question

How should node fencing and split-brain prevention be handled during failover?

## Resolution

Active sidecar fencing with quorum lease: the sidecar constantly tracks Raft heartbeat quorum; if heartbeats lapse beyond the lease duration, the sidecar immediately halts local Postgres via `pg_ctl stop -m immediate` before any standby node can promote.
