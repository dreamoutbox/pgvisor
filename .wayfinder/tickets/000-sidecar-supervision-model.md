---
id: "000-sidecar-supervision-model"
title: "Sidecar Supervision Model"
type: "grilling"
status: "closed"
assignee: "antigravity"
blocked_by: []
---

## Question

How should the sidecar manage the underlying PostgreSQL instance?

## Resolution

Direct Supervisor model: the sidecar runs as container PID 1, executes `initdb`, writes `postgresql.conf` and `pg_hba.conf`, launches and monitors the Postgres child OS process, executes `pg_basebackup` for replica bootstrap, and triggers promotion via `pg_ctl promote`.
