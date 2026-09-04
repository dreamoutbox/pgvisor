---
id: "003"
title: "Sidecar Process Supervision and Signals"
type: "grilling"
status: "open"
assignee: "unassigned"
blocked_by: []
---

## Question

How does the sidecar manage signal forwarding (SIGTERM, SIGINT, SIGQUIT), child process reaping, zombie reaping as container PID 1, pipe logging of Postgres stdout/stderr, and config generation (`postgresql.conf`, `pg_hba.conf`) on startup?
