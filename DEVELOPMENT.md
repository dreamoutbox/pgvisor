# PgVisor Development Guide

This document covers local development, testing, crate architecture, technology stack, and configuration reference for contributors and developers working on PgVisor.

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

## Crate Layout

| Crate | Role |
|---|---|
| `crates/pgvisor-core` | Shared types, PostgreSQL wire protocol codec, pure-Rust OpenRaft storage engine, OpenDAL backup manager |
| `crates/pgvisor-proxy` | L7 transaction connection pooler, read/write splitter, failover buffering |
| `crates/pgvisor-sidecar` | Container PID 1 supervisor, Raft node, automated config generation, backup worker |
| `crates/pgvisor-dashboard` | Axum + Askama web UI, REST API, guarded SQL console, audit log |

---

## Configuration Reference

Cluster configuration is managed via environment variables (see `examples/.env.example`):

### Sidecar Supervisor (`pgvisor-sidecar`)

| Variable | Description | Default |
|---|---|---|
| `PGVISOR_CLUSTER_ID` | Unique cluster identifier | `pgvisor-cluster` |
| `PGVISOR_NODE_ID` | Unique integer node ID (e.g., `1`, `2`, `3`) | Required |
| `PGVISOR_ROLE` | Initial bootstrap role: `leader` or `standby` | `leader` |
| `PGVISOR_PEERS` | Comma-separated sidecar HTTP peer endpoints | Required |
| `PRIMARY_CONNINFO` | Connection string to leader (standby nodes only) | Host address of leader |
| `S3_ENDPOINT` | S3-compatible object storage endpoint | `http://minio:9000` |
| `S3_BUCKET` | Backup bucket name | `pgvisor-backups` |
| `S3_ACCESS_KEY` | Storage access key | `minioadmin` |
| `S3_SECRET_KEY` | Storage secret key | `minioadmin` |
| `PGPORT` | PostgreSQL port | `5432` |
| `PGDATA` | PostgreSQL data directory | `/var/lib/postgresql/data/pgdata` |
| `RUST_LOG` | Logging verbosity (`error`, `warn`, `info`, `debug`) | `info` |

### L7 Proxy & Dashboard (`pgvisor-proxy`)

| Variable | Description | Default |
|---|---|---|
| `PGVISOR_PROXY_LISTEN` | Postgres proxy listen address | `0.0.0.0:5432` |
| `PGVISOR_DASHBOARD_LISTEN` | Web dashboard listen address | `0.0.0.0:8080` |
| `PGVISOR_LEADER_ADDR` | Initial leader PostgreSQL address | `pgvisor-node1:5432` |
| `PGVISOR_STANDBY_ADDRS` | Comma-separated standby PostgreSQL addresses | `pgvisor-node2:5432,pgvisor-node3:5432` |
| `PGVISOR_CLUSTER_ID` | Cluster identifier matching sidecars | `pgvisor-cluster` |
| `PGVISOR_ADMIN_TOKEN` | Bearer token for dashboard authentication | `postgres` |
| `S3_ENDPOINT` | Storage endpoint for backup inspection | `http://minio:9000` |
| `S3_BUCKET` | Backup bucket name | `pgvisor-backups` |
| `S3_ACCESS_KEY` | Storage access key | `minioadmin` |
| `S3_SECRET_KEY` | Storage secret key | `minioadmin` |

---

## Development & Contributing

### Developer Prerequisites

- [Rust toolchain](https://rustup.rs/) (stable)
- [Docker](https://docs.docker.com/get-docker/) & Docker Compose
- `postgresql-client` and `jq`

### Build & Check

```bash
# Verify workspace compiles cleanly
cargo check --workspace

# Run Rust unit tests
cargo test --workspace

# Run specific binary locally
cargo run -p pgvisor-proxy
cargo run -p pgvisor-dashboard
cargo run -p pgvisor-sidecar
```

### Running Integration Tests

Integration tests run against dedicated Docker Compose clusters.

To start or reset the developer test cluster (wiping test volumes and rebuilding images from source):

```bash
./reset-docker-compose.sh
```

> **Note**: `./reset-docker-compose.sh` is an internal developer and CI tool that wipes all persistent test volumes and forces a container image rebuild. For general usage preserving database data, use `./setup.sh`.

Run the full integration test suite:

```bash
# Run all tests sequentially
./test.sh

# Run tests in parallel across isolated compose projects
./test.sh -j 5
```

Individual test suites located in `tests/`:

| Script | What it tests |
|---|---|
| `test-failover.sh` | Leader failure → automatic promotion → proxy rerouting → rejoin as standby |
| `test-backup-restore.sh` | Full basebackup snapshot and restore |
| `test-pitr.sh` | Point-in-time recovery using WAL replay |
| `test-add-node.sh` | Dynamically adding a new standby node |
| `test-transaction.sh` | Transaction correctness through the proxy |
| `test-read-write-split.sh` | Read queries route to replicas, writes route to leader |
| `test-auto-rejoin.sh` | Standby partition recovery and automatic rejoin |
| `test-rejoin-fenced.sh` | Fenced leader rejoin as standby replica verification |

### Project Structure

```
pgvisor/
├── crates/
│   ├── pgvisor-core/       # Shared types, protocol codec, Raft storage, backup
│   ├── pgvisor-proxy/      # L7 wire protocol proxy & connection pooler
│   ├── pgvisor-sidecar/    # PID 1 supervisor, Raft node, backup worker
│   └── pgvisor-dashboard/  # Axum + Askama web UI & REST API
├── examples/               # User / consumer deployment configuration
│   ├── docker-compose.yml  # Production-like 3-node HA cluster compose template
│   ├── .env.example        # Environment variable template for user deployment
│   ├── setup.sh            # Consumer cluster bootstrap & management script
│   └── scripts/            # S3 bucket initializer
├── tests/                  # Integration test scripts
├── scripts/                # Developer and CI helper scripts
├── composes/               # Test-specific Docker Compose profiles
├── docker-compose.yml      # Local dev/test 3-node cluster + MinIO setup
├── reset-docker-compose.sh # Dev helper: wipe volumes, rebuild images, launch test cluster
├── setup.sh                # Root shortcut delegating to examples/setup.sh
├── test.sh                 # Integration test orchestrator
├── Dockerfile              # Multi-binary container image recipe
├── ARCHITECTURE.md         # Architecture specification
└── Cargo.toml              # Workspace manifest
```
