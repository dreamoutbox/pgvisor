# PgVisor

[![CI & Docker Publish](https://github.com/dreamoutbox/pgvisor/actions/workflows/ci.yml/badge.svg)](https://github.com/dreamoutbox/pgvisor/actions/workflows/ci.yml)
[![Docker Image](https://img.shields.io/docker/v/dreamoutbox/pgvisor?sort=semver&label=docker%20image)](https://hub.docker.com/r/dreamoutbox/pgvisor)

**PgVisor** is a lightweight, developer-friendly PostgreSQL High Availability cluster supervisor and proxy — built entirely in pure Rust. It replaces the operational sprawl of Patroni + PgBouncer + Etcd + pgBackRest with a single, unified binary architecture that just works.

> Built for developers who want HA PostgreSQL without the complexity.

---

## Features

### 🔀 L7 PostgreSQL Proxy & Connection Pooling
- **Transaction-level connection pooling** — backends are released back to the pool at transaction boundaries, not session end, dramatically increasing throughput.
- **Read/write splitting** — `SELECT`, `SHOW`, and `EXPLAIN` queries are automatically routed to standby replicas; writes and DDL go strictly to the Raft leader.
- **Failover connection buffering** — during a leader election, in-flight client sessions are transparently paused and replayed against the newly promoted leader without dropping TCP connections.
- Implements the **PostgreSQL 3.0 wire protocol** natively (via [`pgwire`](https://crates.io/crates/pgwire)).

### 🛡️ Automatic High Availability with Split-Brain Prevention
- **OpenRaft-based distributed consensus** — fully peer-to-peer leader election, no external Etcd or Consul required.
- **Quorum Lease fencing** — an isolated leader detects quorum loss within 1,200 ms and immediately fences its local PostgreSQL instance (`pg_ctl stop -m immediate`) — *strictly before* any standby's 1,500 ms election timeout fires — mathematically preventing split-brain writes.
- **Automatic failover and promotion** — a new leader is elected and `pg_ctl promote` is issued without any human intervention.
- **Automatic node rejoin** — a restarted old leader rejoins the cluster as a standby replica automatically.
- **Manual switchover** — initiate a controlled leader transfer from the web dashboard.

### 🗄️ Container Sidecar Supervisor
- Runs as **container PID 1**, properly reaping zombie processes and forwarding `SIGTERM`/`SIGINT`/`SIGQUIT` to PostgreSQL.
- **Automated `postgresql.conf` generation** — WAL archiving, replication slots, and HBA rules configured automatically from environment variables.
- **Standby bootstrapping** — generates `standby.signal` and `primary_conninfo` automatically on replica startup.
- **Cluster scaling** — add new standby nodes at runtime; they clone from the leader via `pg_basebackup` and begin streaming replication.

### ☁️ Continuous Cloud Backup & Point-In-Time Recovery
- **WAL archiving** — every completed 16 MB WAL segment is immediately shipped to object storage.
- **Hourly incremental snapshots** — hourly basebackup delta snapshots.
- **Daily full basebackup** — full physical snapshot at 01:00 UTC.
- **Point-In-Time Recovery (PITR)** — restore to any second in history using a basebackup + WAL replay.
- **Pluggable storage backends** via [OpenDAL](https://github.com/apache/opendal): MinIO, AWS S3, Google Drive, Dropbox, Azure Blob, and local disk.
- **Configurable retention policy** — automated pruning of obsolete snapshots (default: 7 days).

### 🖥️ Web Dashboard
- **Cluster overview** — real-time Raft term, quorum size, node roles, and replication lag.
- **Node inspection** — detailed per-node status, PostgreSQL version, and process uptime.
- **Backup management** — list, trigger, restore, and label basebackup snapshots from the UI.
- **Guarded SQL console** — browser-based read-only SQL REPL with strict security:
  - Comment stripping to block bypass tricks.
  - Multi-statement rejection.
  - Hard cap of 500 rows per query.
  - Statement timeout (default 5 s).
- **Database user & permissions management** — create/delete roles and manage grants from the UI.
- **Audit log** — records node up/down events, backup/restore operations, dangerous SQL (`DROP`/`TRUNCATE`/`DELETE`), election votes and results, and user lifecycle events.
- **Bearer token authentication** via `PGVISOR_ADMIN_TOKEN`.

---

## Architecture

```
PostgreSQL Clients
       │ (Postgres wire protocol :5432)
       ▼
 pgvisor-proxy  ──────────────────────────────────────────────────┐
       │                                                          │
       ├─── Writes ────► Node 1: pgvisor-sidecar + PostgreSQL     │
       │                          (Raft Leader)                   │
       └─── Reads  ────► Node 2: pgvisor-sidecar + PostgreSQL     │
                    ────► Node 3: pgvisor-sidecar + PostgreSQL    │
                                   (Standbys)                     │
                                                                  │
 pgvisor-dashboard (:8080) ◄──────────────────────────────────────┘

 All sidecars ──► OpenDAL ──► MinIO / S3 / Cloud Storage
```

See [ARCHITECTURE.md](ARCHITECTURE.md) for the full specification with sequence diagrams.

### Crate Layout

| Crate | Role |
|---|---|
| `pgvisor-core` | Shared types, PostgreSQL wire protocol codec, pure-Rust OpenRaft storage engine, OpenDAL backup manager |
| `pgvisor-proxy` | L7 transaction connection pooler, read/write splitter, failover buffering |
| `pgvisor-sidecar` | Container PID 1 supervisor, Raft node, automated config generation, backup worker |
| `pgvisor-dashboard` | Axum + Askama web UI, REST API, guarded SQL console, audit log |

---

## Tech Stack

| Layer | Technology |
|---|---|
| **Language** | [Rust](https://www.rust-lang.org/) (2021 edition) |
| **Async Runtime** | [Tokio](https://tokio.rs/) |
| **Distributed Consensus** | [OpenRaft](https://github.com/datafuselabs/openraft) 0.9 |
| **Consensus Storage** | Custom pure-Rust append-only WAL (no RocksDB, no C++ deps) |
| **PostgreSQL Protocol** | [pgwire](https://crates.io/crates/pgwire) 0.41 |
| **Object Storage** | [OpenDAL](https://github.com/apache/opendal) 0.50 (S3, FS, and more) |
| **Web Server** | [Axum](https://github.com/tokio-rs/axum) 0.7 |
| **HTML Templates** | [Askama](https://github.com/djc/askama) 0.12 |
| **Serialization** | Serde + Bincode + `serde_json` |
| **Checksum / Integrity** | CRC32 (via `crc32fast`) |
| **Error Handling** | `thiserror` (libraries) · `anyhow` (binaries) |
| **Logging & Tracing** | `tracing` + `tracing-subscriber` |
| **Dev Object Storage** | [MinIO](https://min.io/) |
| **Containerization** | Docker + Docker Compose |

---

## Getting Started

### Prerequisites

- [Docker](https://docs.docker.com/get-docker/) & Docker Compose
- [Rust toolchain](https://rustup.rs/) (for local development)

### Quick Start (3-Node Cluster)

Spin up a full 3-node HA cluster with MinIO for backup storage in one command:

```bash
./reset-docker-compose.sh
```

This will:
1. Start MinIO and auto-create the `pgvisor-backups` bucket.
2. Build and launch `pgvisor-node1` (leader), `pgvisor-node2`, and `pgvisor-node3` (standbys).
3. Start `pgvisor-proxy` with the web dashboard.

Connect to PostgreSQL through the proxy:

```bash
psql -h localhost -p 5432 -U postgres
```

Open the web dashboard:

```
http://localhost:8080
```

> Default admin token: `postgres` (set via `PGVISOR_ADMIN_TOKEN`)

To restart without rebuilding Docker images:

```bash
./reset-docker-compose.sh --no-build
```

---

## Configuration

All configuration is done via environment variables.

### Sidecar (`pgvisor-sidecar`)

| Variable | Description |
|---|---|
| `PGVISOR_CLUSTER_ID` | Unique cluster identifier |
| `PGVISOR_NODE_ID` | Unique integer node ID (e.g., `1`, `2`, `3`) |
| `PGVISOR_ROLE` | Initial role: `leader` or `standby` |
| `PGVISOR_PEERS` | Comma-separated sidecar HTTP peer addresses |
| `PRIMARY_CONNINFO` | Connection string to leader (standby nodes only) |
| `S3_ENDPOINT` | Object storage endpoint (e.g., `http://minio:9000`) |
| `S3_BUCKET` | Backup bucket name |
| `S3_ACCESS_KEY` | Storage access key |
| `S3_SECRET_KEY` | Storage secret key |
| `PGPORT` | PostgreSQL port (default: `5432`) |
| `PGDATA` | PostgreSQL data directory |
| `RUST_LOG` | Log level (e.g., `info`, `debug`) |

### Proxy (`pgvisor-proxy`)

| Variable | Description |
|---|---|
| `PGVISOR_PROXY_LISTEN` | Proxy listen address (e.g., `0.0.0.0:5432`) |
| `PGVISOR_DASHBOARD_LISTEN` | Dashboard listen address (e.g., `0.0.0.0:8080`) |
| `PGVISOR_LEADER_ADDR` | Leader PostgreSQL address |
| `PGVISOR_STANDBY_ADDRS` | Comma-separated standby PostgreSQL addresses |
| `PGVISOR_CLUSTER_ID` | Cluster identifier |
| `PGVISOR_ADMIN_TOKEN` | Bearer token for dashboard authentication |
| `S3_ENDPOINT` | Object storage endpoint |
| `S3_BUCKET` | Backup bucket name |
| `S3_ACCESS_KEY` | Storage access key |
| `S3_SECRET_KEY` | Storage secret key |

---

## Development

### Build & Check

```bash
# Check all crates compile cleanly
cargo check --workspace

# Run a specific binary locally
cargo run -p pgvisor-proxy
cargo run -p pgvisor-dashboard
cargo run -p pgvisor-sidecar
```

### Running Tests

Tests run against a live Docker Compose cluster. Start the cluster first:

```bash
./reset-docker-compose.sh
```

Then run the test suite:

```bash
# Run all tests sequentially
./test.sh

# Run with parallelism
./test.sh -j 5
```

Individual test scripts in `tests/`:

| Script | What it tests |
|---|---|
| `test-failover.sh` | Leader failure → automatic promotion → proxy rerouting → rejoin as standby |
| `test-backup-restore.sh` | Full basebackup snapshot and restore |
| `test-pitr.sh` | Point-in-time recovery using WAL replay |
| `test-add-node.sh` | Dynamically adding a new standby node |
| `test-transaction.sh` | Transaction correctness through the proxy |
| `test-read-write-split.sh` | Read queries route to replicas, writes route to leader |

> **Note on replication lag**: After a write via the proxy, reads may hit a replica with non-zero replication lag. Test scripts use polling retry loops (5–10 attempts, 1 s sleep) to handle this correctly rather than single-shot assertions.

### Project Structure

```
pgvisor/
├── crates/
│   ├── pgvisor-core/       # Shared types, protocol codec, Raft storage, backup
│   ├── pgvisor-proxy/      # L7 wire protocol proxy & connection pooler
│   ├── pgvisor-sidecar/    # PID 1 supervisor, Raft node, backup worker
│   └── pgvisor-dashboard/  # Axum + Askama web UI & REST API
├── tests/                  # Shell-based integration test scripts
├── scripts/                # Helper scripts (mounted into containers)
├── composes/               # Additional Docker Compose configurations
├── docker-compose.yml      # Local 3-node cluster + MinIO dev setup
├── reset-docker-compose.sh # Wipe volumes and restart the cluster
├── test.sh                 # Test runner
├── Dockerfile              # Multi-binary container image
├── ARCHITECTURE.md         # Full architecture specification
└── Cargo.toml              # Workspace manifest
```

---

## License

[MIT](LICENSE)
