# PgVisor Development Guide

This document covers local development, testing, crate architecture, technology stack, and configuration reference for contributors and developers working on PgVisor.

---

## Tech Stack

| Layer | Technology |
|---|---|
| **Language** | [Rust](https://www.rust-lang.org/) (2024 edition) |
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
| `crates/pgvisor-core` | Shared types, API DTOs, SQL security guard, audit log, PostgreSQL wire protocol codec, pure-Rust OpenRaft storage engine, OpenDAL backup manager |
| `crates/pgvisor-proxy` | SQL-only L7 transaction connection pooler, read/write splitter, failover buffering, `restore_epoch` pool draining |
| `crates/pgvisor-sidecar` | Container PID 1 supervisor, internal `/control/*` (`8080`), authenticated `/api/*` (`8081`), `pg_stat_database` metrics sampler, backup CRON worker |
| `crates/pgvisor-dashboard` | Standalone Axum + Askama web UI (`8080`) connecting directly to `pgvisor-sidecar` `/api/*` via `PGVISOR_SIDECAR_URLS` |

---

## Configuration Reference

Cluster configuration is managed via environment variables (see `docker-compose.yml` and `examples/.env.example`):

### Sidecar Supervisor & API (`pgvisor-sidecar`)

| Variable | Description | Default |
|---|---|---|
| `PGVISOR_CLUSTER_ID` | Unique cluster identifier | `pgvisor-cluster` |
| `PGVISOR_NODE_ID` | Unique integer node ID (e.g., `1`, `2`, `3`) | `1` |
| `PGVISOR_ROLE` | Initial bootstrap role: `leader` or `standby` | `leader` |
| `PGVISOR_PEERS` | Comma-separated sidecar internal `/control` URLs | Empty |
| `PGVISOR_CONTROL_PORT` | Internal `/control/*` HTTP listen port | `8080` |
| `PGVISOR_API_PORT` | Client-facing `/api/*` HTTP listen port | `8081` |
| `POSTGRES_USER` | Primary PostgreSQL superuser role name | `postgres` |
| `POSTGRES_PASSWORD` | Password bootstrapped for `POSTGRES_USER` after `initdb` | None |
| `PGVISOR_SUPERUSER` | Comma-separated PostgreSQL roles allowed to log in to `/api/*` | Value of `POSTGRES_USER` (`postgres`) |
| `PGVISOR_CLUSTER_SECRET` | Shared HMAC secret for `/control/*` auth (and fallback token signer) | None |
| `PGVISOR_API_SESSION_SECRET` | HMAC secret for signing `/api/*` session tokens across sidecars | Falls back to `PGVISOR_CLUSTER_SECRET` / random |
| `PGVISOR_SIDECAR_BACKUP_CRON` | Enable automated backup CRON scheduling in sidecar | Enabled when `cron_enabled` is true unless `false`/`0`/`no` |
| `PGVISOR_DASHBOARD_ALLOW_MUTATIONS` | Allow SQL mutations in `/api/sql` (`false` enforces read-only guard) | `true` |
| `PRIMARY_CONNINFO` | Connection string to leader (standby nodes only) | None |
| `S3_ENDPOINT` / `PGVISOR_S3_ENDPOINT` | S3-compatible object storage endpoint | `http://minio:9000` |
| `S3_BUCKET` / `PGVISOR_S3_BUCKET` | Backup bucket name | `pgvisor-backups` |
| `S3_ACCESS_KEY` | Storage access key | `minioadmin` |
| `S3_SECRET_KEY` | Storage secret key | `minioadmin` |
| `PGPORT` | PostgreSQL port | `5432` |
| `PGDATA` | PostgreSQL data directory | `/var/lib/postgresql/data` |
| `RUST_LOG` | Logging verbosity (`error`, `warn`, `info`, `debug`) | `info` |

### L7 SQL Proxy (`pgvisor-proxy`)

| Variable | Description | Default |
|---|---|---|
| `PGVISOR_PROXY_LISTEN` | Postgres SQL proxy listen address | `0.0.0.0:5432` |
| `PGVISOR_LEADER_ADDR` | Initial leader PostgreSQL address | `pgvisor-node1:5432` |
| `PGVISOR_STANDBY_ADDRS` | Comma-separated standby PostgreSQL addresses | Empty |
| `PGVISOR_CONTROL_PORT` | Sidecar control port polled for topology and `restore_epoch` | `8080` |
| `PGVISOR_CLUSTER_SECRET` | Shared HMAC secret matching sidecar `/control/*` auth | None |

### Standalone Web Dashboard (`pgvisor-dashboard`)

| Variable | Description | Default |
|---|---|---|
| `PGVISOR_SIDECAR_URLS` | Comma-separated sidecar `/api` base URLs | `http://127.0.0.1:8081` |
| `PGVISOR_DASHBOARD_LISTEN` | Dashboard HTTP listen address | `0.0.0.0:8080` |
| `PGVISOR_CLUSTER_ID` | Display cluster identifier for SSR views | `pgvisor-primary` |

---

## Sidecar `/api` Contract (Dashboard & Future CLI)

Both `pgvisor-dashboard` and future CLI clients talk directly to `pgvisor-sidecar` `/api/*` (`PGVISOR_API_PORT`, default `8081`):

1. **Discovery / Connection String**: Clients take a comma-separated list of sidecar API URLs (`PGVISOR_SIDECAR_URLS`).
2. **Authentication**:
   - `POST /api/auth/login` with `{"username": "...", "password": "..."}`. Returns `403 Forbidden` if username is not in `PGVISOR_SUPERUSER`, `401 Unauthorized` if password fails SCRAM-SHA-256 / MD5 verification against PostgreSQL, or `200 OK` with `{"token", "username", "expires_at"}` (plus `Set-Cookie: pgvisor_token=...`).
   - Subsequent requests pass `Authorization: Bearer <token>` (checked via `GET /api/auth/me`).
3. **Leader Discovery (`GET /api/cluster/members`)**: Returns `{"leader_url": "...", "members": [{"node_id", "api_url", "role", "health", "is_self"}]}` so clients cache the active leader URL and fail over across listed sidecars.
4. **`NotLeader` Retry Rule**:
   - Read calls (`GET`/`HEAD`, read-only SQL) can target any healthy sidecar.
   - Cluster-mutating requests must reach the leader. If sent to a standby, the sidecar returns `409 Conflict` with `{"status": "not_leader", "leader_url": "..."}`. Clients update their cached leader URL (or re-query `/api/cluster/members`) and retry once against the leader.

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
cargo nextest run --test-threads 1 --fail-fast

# Run specific binary locally
cargo run -p pgvisor-sidecar
cargo run -p pgvisor-proxy
PGVISOR_SIDECAR_URLS=http://127.0.0.1:8081 cargo run -p pgvisor-dashboard
```

### Running Integration Tests

Integration tests run against dedicated Docker Compose clusters.

To start or reset the developer test cluster (wiping test volumes and rebuilding images from source):

```bash
./reset-docker-compose.sh
```

> **Note**: `./dev-reset-docker-compose.sh` is an internal developer and CI tool that wipes all persistent test volumes and forces a container image rebuild. For general usage preserving database data, use `./pgvisorctl.sh`.

Run the full integration test suite:

```bash
# Run all tests sequentially
./test.sh

# Run tests in parallel across isolated compose projects
./test.sh -j 4
```

Individual test suites located in `tests/`

### Developer Utilities (S3 Snapshots & Backup Inspection)

For debugging, bug reproduction, and inspecting backup metadata:

- **Export S3 Backups to Local Directory**:
  ```bash
  ./dev-take-backups-snapshot-s3.sh
  # Dumps all objects from S3 to ./debug-s3-bucket-snapshot
  ```
- **Import S3 Backups from Local Directory**:
  ```bash
  ./dev-import-backups-snapshot-s3.sh --clean
  # Wipes the target S3 bucket and restores objects from ./debug-s3-bucket-snapshot
  ```
- **Inspect Backup Metadata**:
  ```bash
  ./dev-inspect-backup.sh
  # Formats backup metadata (snapshot ID, label, created at, last transaction time, and last transaction xid) in an aligned table.
  # Use --dir=./debug-s3-bucket-snapshot for offline inspection, or pass snapshot-id and --json for machine-readable output.
  ```
- **Inspect Cluster Timelines & Divergence**:
  ```bash
  ./dev-inspect-timeline.sh
  # Inspects timeline IDs, checkpoint LSNs, replication roles, and timeline history across all cluster nodes.
  # Detects timeline divergence, prints branching lineage, and supports --json and --s3 flags.
  ```

### Project Structure

```
pgvisor/
├── crates/
│   ├── pgvisor-core/       # Shared types, protocol codec, Raft storage, backup
│   ├── pgvisor-proxy/      # SQL-only L7 wire protocol proxy & connection pooler
│   ├── pgvisor-sidecar/    # PID 1 supervisor, /control (:8080), /api (:8081), backup worker
│   └── pgvisor-dashboard/  # Standalone Axum + Askama web UI & SidecarClient
├── examples/               # User / consumer deployment configuration
│   ├── docker-compose.yml  # 3 nodes HA cluster compose template
│   ├── .env.example        # Environment variable template for user deployment
│   ├── pgvisorctl.sh       # Consumer cluster bootstrap & management script
│   └── scripts/            # S3 bucket initializer
├── tests/                  # Integration test scripts
├── scripts/                # Developer and CI helper scripts
├── composes/               # Test-specific Docker Compose profiles
├── docker-compose.yml      # Local dev/test 3-node cluster + MinIO setup
├── reset-docker-compose.sh # Dev helper: wipe volumes, rebuild images, launch test cluster
├── test.sh                 # Integration test orchestrator
├── Dockerfile              # Multi-binary container image recipe
├── ARCHITECTURE.md         # Architecture specification
└── Cargo.toml              # Workspace manifest
```
