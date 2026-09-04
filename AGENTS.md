# AGENTS.md - PgVisor Developer & Agent Guide

## Project Overview

**PgVisor** is a lightweight, simplified PostgreSQL High Availability (HA) cluster supervisor and proxy built in Rust. It eliminates the complexity of traditional PostgreSQL cluster setups for developers through:
- **L7 Connection Proxy (`pgvisor-proxy`)**: PostgreSQL wire protocol proxy supporting transaction-level connection pooling and read/write splitting (writes to Raft leader, reads to standby replicas) with failover connection buffering.
- **Sidecar Supervisor (`pgvisor-sidecar`)**: Container PID 1 process managing the local PostgreSQL instance (`initdb`, configuration generation, process supervision, healthchecks, Raft quorum lease fencing via `pg_ctl stop -m immediate`, and promotion via `pg_ctl promote`).
- **Distributed Consensus**: OpenRaft-based HA election and membership using a custom pure-Rust append-only storage engine.
- **Continuous Backups**: Physical basebackup snapshots and continuous WAL archiving (`archive_command`) to S3/MinIO/cloud storage via OpenDAL, supporting Point-In-Time-Recovery (PITR).
- **Web Dashboard (`pgvisor-dashboard`)**: Axum + Askama UI embedded in the proxy service for cluster status, node inspection, backup management, and SQL execution.

---

## Planned Project Structure

```text
pgvisor/
├── .wayfinder/                      # Wayfinder roadmap, decisions & frontier tickets
│   ├── map.md                       # Canonical map index
│   └── tickets/                     # Open and closed decision tickets
├── crates/
│   ├── pgvisor-core/                # Shared types, Raft state machine, storage traits, wire protocol common
│   ├── pgvisor-proxy/               # L7 Postgres wire protocol proxy & connection pooler
│   ├── pgvisor-sidecar/             # PID 1 Postgres process supervisor, Raft node, OpenDAL backup worker
│   └── pgvisor-dashboard/           # Axum + Askama web UI and API handlers
├── docker-compose.yml               # Local MinIO dev setup & cluster compose
├── reset-docker-compose.sh          # Helper to wipe volumes and restart compose services
├── THIS-PROJECT-GOAL.md             # High-level product requirements
├── AGENTS.md                        # This file (guidelines & cautions for coding agents)
└── Cargo.toml                       # Workspace manifest
```

---

## Cautions & Rules for AI Agents

### 1. Rust Build & Test Rules
- **Write Tests to Prove Changes**: Always write unit and/or integration tests to prove that changes and new features work as expected and prevent regressions.
- **Run `cargo check` after edits**: Verify that changes compile cleanly.
- **Avoid `ref` / `ref mut` in patterns**: Rely on Rust 2018+ match ergonomics. Use `.as_ref()` or `.as_mut()` instead of `ref` / `ref mut`.
- **Strict Enum Typing**: Always use `enum` for any closed set of variants (e.g. node states, replication roles, Raft message kinds). Never use magic strings.
- **Intent-Focused Comments**: Comment purpose/intent of functions and blocks. Avoid line-by-line noise comments.
- **Plain-Text Test Scripts (No ANSI Colors)**: All test, benchmark, and verification scripts must output clean plain text. Do not use ANSI color escape sequences (`\033[...]`, `tput`, color variables).

### 2. Error Handling Constraints
- **Library Crates (`pgvisor-core`, internal libs)**: Use `thiserror` to define explicit domain error types. Never use `anyhow` in public library APIs.
- **Binary Crates (`pgvisor-proxy`, `pgvisor-sidecar`, `pgvisor-dashboard`)**: `anyhow::Result` is allowed only at binary boundaries (`main.rs`, CLI entrypoints).
- **No `unwrap()` or `expect()`**: Propagate errors with `?`. If truly unavoidable due to a compiler-invisible invariant, write a comment justifying why it cannot panic.
- **Never swallow errors silently**: Avoid `let _ = ...` on fallible operations unless explicitly intentional, with a comment explaining why.

### 3. Wayfinder Decision Workflow
- Consult [.wayfinder/map.md](.wayfinder/map.md) before making architectural choices.
- Any unresolved architectural question must be addressed through a ticket in `.wayfinder/tickets/`.
- Never resolve more than one ticket per session.

### 4. Technical Gotchas
- **OpenRaft Storage**: Do not pull in RocksDB or external C++ dependencies. PgVisor builds its own custom pure-Rust append-only storage engine.
- **Package Manager**: If any web assets or frontend tools are used, use `pnpm` exclusively (never `npm` or `yarn`).
- **PID 1 Responsibilities in Sidecar**: The sidecar acts as container PID 1. It must properly reap child zombie processes (`waitpid`) and propagate signals (`SIGTERM`, `SIGINT`, `SIGQUIT`) to Postgres.
- **Fencing over Promotion**: During network splits, the old leader must be fenced (killed via `pg_ctl stop -m immediate`) before or concurrently with standby promotion to prevent split-brain data corruption.
- **Cluster Restore & Standby Timeline Realignment**: Restoring a database snapshot rewinds the leader's timeline and LSN. Standby replicas cannot resume replication without re-cloning from the restored leader. Cluster restores must be coordinated through the sidecar control API:
  1. Restore leader node via `POST /control/restore`.
  2. Re-sync all standby replicas via `POST /control/resync` (`pg_basebackup`).
  3. Drain proxy connection pool (`pool.drain_all()`) so client connections refresh to the restored timeline.
- **Sidecar Supervision Boundaries (No Out-of-Band `pg_ctl`)**: When `pgvisor-sidecar` runs as container PID 1, never execute out-of-band `pg_ctl stop`/`pg_ctl start` commands via shell scripts or `docker exec`. External stops corrupt process accounting and supervisor state. Always issue lifecycle and restore commands through the sidecar's internal HTTP control API.
- **Cluster Reset & Docker Compose Helper (`reset-docker-compose.sh`)**: Running `./reset-docker-compose.sh` wipes volumes and restarts containers with fresh data without rebuilding images. Pass `--build` (`./reset-docker-compose.sh --build`) only when binary or configuration code changes require rebuilding images.
