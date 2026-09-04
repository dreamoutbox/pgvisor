---
id: "000-backup-and-pitr-strategy"
title: "Backup & PITR Strategy"
type: "grilling"
status: "closed"
assignee: "antigravity"
blocked_by: []
---

## Question

What backup and restore strategy should the sidecar use with OpenDAL?

## Resolution

Continuous physical backup: periodic `pg_basebackup` snapshots combined with continuous WAL archiving via PostgreSQL `archive_command` pushing segments to OpenDAL, enabling full Point-In-Time-Recovery (PITR).
