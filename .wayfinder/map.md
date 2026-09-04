# Wayfinder Map: PgVisor Architecture & Specification

## Destination

A comprehensive, vetted architecture and specification document (`ARCHITECTURE.md`) defining PgVisor's crate layout, L7 PostgreSQL wire protocol proxying with transaction pooling, custom pure-Rust OpenRaft storage & failover orchestration, sidecar process supervision, OpenDAL physical backup/PITR pipeline, and Axum + Askama dashboard.

## Notes

- **Domain**: PostgreSQL High Availability cluster supervisor, L7 connection proxy, distributed consensus (Raft), cloud storage backup (OpenDAL), web management dashboard.
- **Skills to consult**: `grilling`, `wayfinder`, `prototype`.
- **Standing preferences & constraints**:
  - Rust Tokio async runtime.
  - Cargo workspace with separate standalone binaries: `pgvisor-proxy`, `pgvisor-sidecar`, `pgvisor-dashboard`, sharing `pgvisor-core`.
  - Error handling: `thiserror` for library crates, `anyhow` for binary boundaries (`main`, CLI handlers). No unwrap/expect in production code.
  - Strict type modeling: Use `enum` for any closed set of variants (node roles, cluster states, Raft message types, backup statuses).
  - Match ergonomics: Avoid `ref` and `ref mut` in patterns.
  - Package manager: `pnpm` only (if any web build tooling is ever needed).
  - Testing: Do not run `cargo test` automatically (user runs tests); run `cargo check` to verify compilation.

## Decisions so far

- [Destination Scope](tickets/000-destination-scope.md): Produce a comprehensive architecture & specification document (`ARCHITECTURE.md`) before starting code implementation.
- [L7 Protocol & Pooling Mode](tickets/000-l7-protocol-and-pooling.md): L7 Postgres wire protocol proxy with read/write splitting and transaction-level connection pooling.
- [Sidecar Supervision Model](tickets/000-sidecar-supervision-model.md): Sidecar runs as direct container PID 1 supervisor managing Postgres lifecycle (`initdb`, configs, spawn, monitor, promote).
- [Dashboard Architecture](tickets/000-dashboard-architecture.md): Axum + Askama web dashboard embedded in the proxy service as a central cluster gateway.
- [Crate & Binary Packaging](tickets/000-crate-and-binary-packaging.md): Cargo workspace with separate standalone binaries (`pgvisor-proxy`, `pgvisor-sidecar`, `pgvisor-dashboard`) sharing `pgvisor-core`.
- [OpenRaft Storage Strategy](tickets/000-openraft-storage-strategy.md): Build our own custom pure-Rust append-only storage engine for OpenRaft logs and state machine.
- [Backup & PITR Strategy](tickets/000-backup-and-pitr-strategy.md): Continuous physical backup via periodic `pg_basebackup` snapshots + continuous WAL archiving to OpenDAL supporting PITR.
- [Fencing & Split-Brain Prevention](tickets/000-fencing-and-split-brain-prevention.md): Active sidecar fencing with quorum lease: sidecar immediately halts Postgres (`pg_ctl stop -m immediate`) if quorum heartbeats are lost.
- [Bootstrapping & Discovery](tickets/000-bootstrapping-and-discovery.md): Static configuration / environment peer seed list; node-1 bootstraps cluster on first boot.
- [Custom OpenRaft Storage Engine](tickets/001-custom-openraft-storage-engine-design.md): Implemented pure-Rust append-only WAL with CRC32 integrity, in-memory index, atomic state machine snapshots, and OpenRaft storage traits.
- [Postgres Wire Protocol & Pooling](tickets/002-postgres-wire-protocol-framing-and-pooling.md): Implemented wire protocol 3.0 framing, transaction status tracking ('I'/'T'/'E'), read/write splitting, and transaction-level connection pooling with failover draining.
- [Sidecar Process Supervision](tickets/003-sidecar-process-supervision-and-signals.md): Implemented container PID 1 signal trapping, config templating, stdout/stderr pipe logging, standby promotion, and quorum fencing via pg_ctl stop -m immediate.
- [Raft Failover Orchestration](tickets/004-raft-state-machine-and-failover-orchestration.md): Implemented QuorumLease (1200ms) with proactive leader fencing before standby election timeout, and FailoverOrchestrator for promotion and recovery.
- [OpenDAL Backup & WAL Archiving](tickets/005-opendal-wal-archiving-and-basebackup-pipeline.md): Implemented BackupManager with OpenDAL for continuous WAL archiving, basebackup snapshots, and PITR retention pruning.
- [Proxy Failover Buffering & Reconnection](tickets/006-proxy-failover-buffering-and-reconnect.md): Implemented topology watch notifications, client query buffering during Raft elections, transparent query retry before response dispatch, and safe ErrorResponse fallback.
- [Dashboard API & SQL Console Security](tickets/007-dashboard-api-and-sql-console-security.md): Implemented Axum + Askama web UI and JSON status API, SQL console read-only AST filtering, statement timeouts, admin token auth, and MinIO dev default backup schedule (hourly incremental, full after midnight).
- [Synthesize Architecture Spec](tickets/008-synthesize-architecture-spec.md): Synthesized and formalized the canonical [ARCHITECTURE.md](../ARCHITECTURE.md) covering crate architecture, consensus & pure-Rust WAL storage, L7 wire protocol pooling & failover buffering, container PID 1 supervisor model, and MinIO cloud backup pipeline.

## Not yet specified

- **Dynamic Cluster Scaling**: Protocol for adding and removing sidecar nodes dynamically via OpenRaft joint consensus at runtime without node restarts.
- **Cross-Region Replication**: Topology and lag-handling for read-only standbys across distinct geographic regions.
- **Cluster Observability & Metrics**: Prometheus exporter schema for OpenRaft consensus latency and PostgreSQL replication lag.
- **Backup Encryption & Compression**: Client-side Zstd compression and ChaCha20-Poly1305 encryption before writing WAL and basebackup chunks to OpenDAL.

## Out of scope

- **Kubernetes Operator**: Custom CRDs and k8s-native controllers; focus is Docker and bare-metal deployments first.
- **Distributed SQL / Sharding**: Citus-like distributed partitioning; PgVisor targets single-leader active-standby HA.
- **Active-Active Multi-Master**: Multi-master write conflict resolution; PgVisor strictly enforces a single active read-write leader.
