# PgVisor

[![CI & Docker Publish](https://github.com/dreamoutbox/pgvisor/actions/workflows/ci.yml/badge.svg)](https://github.com/dreamoutbox/pgvisor/actions/workflows/ci.yml)
[![Docker Image](https://img.shields.io/docker/v/dreamoutbox/pgvisor?sort=semver&label=docker%20image)](https://hub.docker.com/r/dreamoutbox/pgvisor)

**PgVisor** is a lightweight, developer-friendly PostgreSQL High Availability cluster supervisor and proxy — built entirely in pure Rust. It replaces the operational sprawl of Patroni + PgBouncer + Etcd + pgBackRest with a single, unified binary architecture that just works.

> Built for developers who want HA PostgreSQL without the complexity.

---

## Features

- **🔀 L7 Connection Proxy** — Native PostgreSQL wire protocol proxy with transaction-level pooling, transparent read/write splitting (writes to leader, reads to standbys), and zero-downtime failover request buffering.
- **🛡️ Raft High Availability** — Peer-to-peer OpenRaft consensus with Quorum Lease fencing (`pg_ctl stop -m immediate` in 1,200ms) to strictly eliminate split-brain writes without Etcd or Consul.
- **🗄️ Sidecar Supervisor** — Container process reaper and supervisor handling automated PostgreSQL config generation, replica bootstrapping, and dynamic cluster scaling.
- **☁️ Continuous Backup & PITR** — Streaming WAL archiving and automated basebackups via OpenDAL (MinIO, S3, R2, GCS) with second-precision Point-In-Time-Recovery.
- **🖥️ Web Management Dashboard** — Embedded UI and REST API for real-time cluster topology, live metrics, manual switchover, guarded read-only SQL console, and audit logging.

---

## Architecture

```mermaid
flowchart TD
    Clients["PostgreSQL Clients"] -->|":5432 Wire Protocol"| Proxy["pgvisor-proxy"]
    User["Web Browser"] -->|":8080 HTTP"| Proxy

    subgraph Cluster ["3-Node PostgreSQL Cluster"]
        Proxy -->|"Writes & Transactions"| Node1["Node 1 (Raft Leader)"]
        Proxy -->|"Read Queries"| Node2["Node 2 (Standby Replica)"]
        Proxy -->|"Read Queries"| Node3["Node 3 (Standby Replica)"]
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

PgVisor provides a production-like 3-node PostgreSQL 18 High-Availability (HA) cluster with an L7 connection proxy, automatic Raft consensus failover, continuous S3/MinIO backup archiving, and an embedded web dashboard.

### Prerequisites (For Users)

- [Docker](https://docs.docker.com/get-docker/) Engine (24.0+) & Docker Compose v2+
- *(Optional)* `psql` command-line client to connect directly

---

### Quick Start (Bootstrap in One Command)

To bootstrap a complete 3-node HA PostgreSQL cluster with S3 backup storage and L7 proxy:

```bash
./setup.sh
```

*(Alternatively, from inside the `examples/` directory: `cd examples && ./setup.sh`)*

The bootstrap script will:
1. Initialize environment configuration from `examples/.env.example` into `examples/.env`.
2. Launch MinIO/RustFS object storage and auto-create the `pgvisor-backups` bucket.
3. Start `pgvisor-node1` (Leader), `pgvisor-node2` (Standby Replica), and `pgvisor-node3` (Standby Replica).
4. Start `pgvisor-proxy` on port `5432` with the web management dashboard on port `8080`.
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

Open the embedded web dashboard in your browser:

```
http://localhost:8080
```

- **Authentication**: Enter your admin token (default: `postgres`, configured via `PGVISOR_ADMIN_TOKEN` in `examples/.env`).
- **Cluster Overview**: Live cluster topology, node roles (Leader vs. Standby), consensus status, and replication lag.
- **Manual Failover & Switchover**: Safely initiate graceful leader step-down or switchover from the UI.
- **Backup & Restore**: View basebackup snapshots, trigger on-demand physical backups, and execute Point-In-Time-Recovery (PITR).
- **SQL Runner & Audit Logs**: Interactive read-guarded SQL console and cluster audit logging (elections, node join/leave, DDL events).

---

### Cluster Lifecycle & Operations

Manage your cluster using `./setup.sh`:

```bash
# View live container logs
./setup.sh logs

# View logs of a specific service (e.g., proxy or node1)
./setup.sh logs pgvisor-proxy

# Check cluster status and node health
./setup.sh status

# Restart the cluster containers
./setup.sh restart

# Stop the cluster (all database volumes and data preserved)
./setup.sh down

# Stop the cluster and wipe persistent data volumes (clean reset)
./setup.sh clean
```

---

## Development & Configuration

For local development setup, testing workflows, crate architecture, technology stack, and the full environment variable reference, see **[DEVELOPMENT.md](DEVELOPMENT.md)**.

---

## License

[MIT](LICENSE)

