# PgVisor

[![CI & Docker Publish](https://github.com/dreamoutbox/pgvisor/actions/workflows/ci.yml/badge.svg)](https://github.com/dreamoutbox/pgvisor/actions/workflows/ci.yml)
[![Docker Image](https://img.shields.io/docker/v/dreamoutbox/pgvisor?sort=semver&label=docker%20image)](https://hub.docker.com/r/dreamoutbox/pgvisor)

**PgVisor** is a lightweight, developer-friendly PostgreSQL High Availability cluster supervisor and proxy — built in Rust. It replaces the operational sprawl of Patroni + PgBouncer + Etcd + pgBackRest with a single, unified binary architecture that just works.

![pgvisor web dashboard overview page](pgvisor-images/overview-1.png)

---

## Features

- **🔀 L7 Connection Proxy** — Native PostgreSQL wire protocol proxy with transaction-level pooling, transparent read/write splitting (writes to leader, reads to standbys), and zero-downtime failover request buffering.
- **🛡️ Raft High Availability** — Peer-to-peer OpenRaft consensus with Quorum Lease fencing (`pg_ctl stop -m immediate` in 1,200ms) to strictly eliminate split-brain writes without Etcd or Consul.
- **🗄️ Sidecar Supervisor** — Container process reaper and supervisor handling automated PostgreSQL config generation, replica bootstrapping, and dynamic cluster scaling.
- **☁️ Continuous Backup & PITR** — Streaming WAL archiving and automated basebackups via OpenDAL (MinIO, S3, R2, GCS) with second-precision Point-In-Time-Recovery.
- **Web Management Dashboard (`pgvisor-dashboard`)** — Standalone web UI communicating directly with sidecar `/api/*` endpoints (`8081`) for real-time cluster topology, live metrics, manual switchover, guarded SQL console, user management, and audit logging.

---

## Architecture

```mermaid
flowchart TD
    Clients["PostgreSQL Clients"] -->|":5432 Wire Protocol"| Proxy["pgvisor-proxy (SQL-Only)"]
    User["Web Browser"] -->|":8080 HTTP"| Dashboard["pgvisor-dashboard (Standalone)"]

    subgraph Cluster ["3-Node PostgreSQL Cluster"]
        Proxy -->|"Writes & Transactions"| Node1["Node 1 (Leader)"]
        Proxy -->|"Read Queries"| Node2["Node 2 (Standby Replica)"]
        Proxy -->|"Read Queries"| Node3["Node 3 (Standby Replica)"]
        Dashboard -->|":8081 /api/*"| Node1
        Dashboard -.->|":8081 /api/* Failover"| Node2
        Node1 -.->|"Streaming Replication"| Node2
        Node1 -.->|"Streaming Replication"| Node3
    end

    Node1 -->|"WAL Archiving & Basebackups"| S3["Object Storage (S3 / MinIO / R2)"]
    Node2 -.->|"Restore / PITR"| S3
    Node3 -.->|"Restore / PITR"| S3
```

See [ARCHITECTURE.md](ARCHITECTURE.md) for full architecture specifications and sequence diagrams.

---

## Getting Started

PgVisor provides a PostgreSQL 18 High-Availability (HA) cluster with a SQL-only L7 connection proxy, automatic failover, continuous S3/SeaweedFS/MinIO backup archiving, and a standalone web dashboard talking directly to the sidecar API.

### Prerequisites (For Users)

- [Docker](https://docs.docker.com/get-docker/) Engine (24.0+) & Docker Compose v2+
- *(Optional)* `psql` command-line client to connect directly

---

### Quick Start (Bootstrap in One Command)

To bootstrap a complete 3-node HA PostgreSQL cluster with S3 backup storage and L7 proxy:

```bash
cd examples/
./pgvisorctl.sh
```


The bootstrap script will:
1. Initialize environment configuration from `examples/.env.example` into `examples/.env`.
2. Launch MinIO/RustFS object storage and auto-create the `pgvisor-backups` bucket.
3. Start `pgvisor-node1` (Leader), `pgvisor-node2` (Standby Replica), and `pgvisor-node3` (Standby Replica).
4. Start `pgvisor-proxy` (SQL-only) on port `5432` and standalone `pgvisor-dashboard` on port `8080` (connecting to sidecar `/api/*` on port `8081`).
5. Wait for all cluster health checks to report healthy and output connection details.

---

### Running via Docker Compose Directly

If you prefer using Docker Compose without the bootstrap script:

```bash
# Start cluster in background
docker compose -f examples/docker-compose.yml up -d

# Check cluster health
docker compose -f examples/docker-compose.yml ps
```

---

### Connecting to the Cluster

Connect your applications and database tools to the **PgVisor L7 Proxy** on port `5432`:

#### Via `psql` CLI

```bash
psql -h localhost -p 5432 -U postgres -d postgres
```

#### Application Connection URI

```text
postgresql://postgres:postgres@localhost:5432/postgres
```

#### Transparent Read/Write Splitting & Zero-Downtime Failover

- **Write Queries**: Mutating queries (`INSERT`, `UPDATE`, `DELETE`, `CREATE`, `DROP`, `ALTER`) and explicit transactions are automatically routed to the current Raft leader (`pgvisor-node1`).
- **Read Queries**: Read-only queries (`SELECT`) are automatically distributed across standby replicas (`pgvisor-node2`, `pgvisor-node3`) with connection pooling.
- **Failover Buffering**: If the leader node crashes or becomes partitioned, the proxy holds in-flight requests in memory while the remaining nodes elect a new leader via OpenRaft (<3 seconds). Pending queries are replayed to the new leader without dropping client TCP connections.

---

### Web Management Dashboard

Open the standalone web dashboard in your browser:

```
http://localhost:8080
```

- **Authentication**: Sign in with a PostgreSQL username/password that is allowed in `PGVISOR_SUPERUSER` (default user: `postgres`, password from `POSTGRES_PASSWORD`). Non-superusers receive `403 Forbidden`.
- **Cluster Overview**: Live cluster topology, node roles (Leader vs. Standby), consensus status, and replication lag.
- **Manual Failover & Switchover**: Safely initiate graceful leader step-down or switchover from the UI.
- **Backup & Restore**: View basebackup snapshots, trigger on-demand physical backups, and execute Point-In-Time-Recovery (PITR).
- **SQL Runner & Audit Logs**: Interactive SQL console, database/table explorer, role management, and cluster audit logging.

---

### Cluster Lifecycle & Operations

Manage your cluster using `./pgvisorctl.sh`:

```bash
# View live container logs
./pgvisorctl.sh logs

# View logs of a specific service (e.g., proxy or node1)
./pgvisorctl.sh logs pgvisor-proxy

# Check cluster status and node health
./pgvisorctl.sh status

# Restart the cluster containers
./pgvisorctl.sh restart

# Stop the cluster (all database volumes and data preserved)
./pgvisorctl.sh down

# Stop the cluster and wipe persistent data volumes (clean reset)
./pgvisorctl.sh clean
```

---

## Development & Configuration

For local development setup, testing workflows, crate architecture, technology stack, and the full environment variable reference, see **[DEVELOPMENT.md](DEVELOPMENT.md)**.

---

## License

[MIT](LICENSE)

