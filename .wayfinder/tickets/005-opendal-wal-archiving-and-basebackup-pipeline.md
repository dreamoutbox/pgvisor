---
id: "005"
title: "OpenDAL WAL Archiving and Basebackup Pipeline"
type: "research"
status: "open"
assignee: "unassigned"
blocked_by: ["003"]
---

## Question

How does PostgreSQL's `archive_command` / `restore_command` interface with the sidecar's OpenDAL worker (e.g., local CLI helper vs Unix domain socket IPC), how are `pg_basebackup` snapshots triggered/streamed, and how are snapshot metadata and WAL retention policies structured?
