---
id: "008"
title: "Synthesize Architecture Spec"
type: "task"
status: "closed"
assignee: "antigravity"
blocked_by: ["001", "002", "003", "004", "005", "006", "007"]
---

## Question

Assemble and formalize all settled decisions, data models, message flows, sequence diagrams, and crate interfaces into the final `ARCHITECTURE.md` specification and implementation milestone plan.

## Resolution

Synthesized and formalized the canonical [ARCHITECTURE.md](../../ARCHITECTURE.md) covering:
1. **System Overview & Mission Statement**: Unified Rust binary replacement for Patroni + PgBouncer + Consul + pgBackRest.
2. **Workspace & Crate Responsibility Matrix**: `pgvisor-core`, `pgvisor-proxy`, `pgvisor-sidecar`, `pgvisor-dashboard`.
3. **Consensus & Storage Engine**: Custom append-only WAL format with CRC32 integrity check, in-memory index, atomic state machine snapshots, and 1200ms Quorum Lease fencing strictly before 1500ms election timeout.
4. **L7 Wire Protocol & Transaction Pooling**: ReadyForQuery status tracking ('I'/'T'/'E'), connection borrowing per transaction, read/write query routing, failover query buffering, and transparent replay.
5. **Container PID 1 Supervisor Model**: Signal trapping, zombie process reaping, automated configuration generation.
6. **Continuous Backup Pipeline**: OpenDAL integration defaulting to MinIO dev S3 (`http://127.0.0.1:9000`), continuous WAL archiving, hourly incremental base snapshots, and full basebackup after midnight (01:00 UTC).
7. **Web Dashboard & SQL Console Security**: Axum + Askama web UI, comment stripping, multi-statement rejection, read-only AST enforcement, and statement timeouts.
