# PgVisor Python Demo Application (MVC + Bootstrap 5)

A containerized Python web application built using the **Model-View-Controller (MVC)** architectural pattern and **Bootstrap 5.3**. It demonstrates connecting to a PostgreSQL High Availability cluster managed by **PgVisor** via its L7 wire protocol proxy (`pgvisor-proxy:5432`).

---

## Key Highlights

- **MVC Architecture**: Clean separation between Data Models (`models/`), Presentation Views (`views/`), and Business Logic Controllers (`controllers/`).
- **PgVisor L7 Proxy Integration**: Demonstrates read/write splitting where `SELECT` queries route to read-only standby replicas and mutating transactions (`INSERT`, `UPDATE`, `DELETE`) route to the Raft leader.
- **Failover-Resilient Connection Pool**: Employs a thread-safe connection pool with automatic retry to seamlessly survive transparent proxy failover buffering.
- **Bootstrap 5 UI**: Clean, responsive interface featuring metrics cards, status badges, search and filter toolbar, modal dialogs, and a cluster diagnostic panel.
- **Production Containerization**: Multi-stage/slim Docker image (`python:3.12-slim`), non-root execution (`appuser`), and Gunicorn WSGI server.

---

## Directory Structure

```text
examples/demo_app/
├── Dockerfile                 # Slim container image with unprivileged appuser
├── docker-compose.yml         # Compose configuration for the demo app
├── .env.example               # Environment variable defaults
├── README.md                  # This documentation
├── requirements.txt           # Flask, psycopg2-binary, gunicorn, python-dotenv
├── app.py                     # Application factory & runner
├── config.py                  # Database & environment configuration
├── models/
│   ├── __init__.py            # Model exports
│   ├── database.py            # Threaded connection pool & query/write helpers
│   ├── item.py                # CRUD Data Model for demo_items
│   └── cluster.py             # Cluster metadata & read/write routing inspection
├── controllers/
│   ├── __init__.py            # Controller exports
│   ├── item_controller.py     # Web routes for item listing, creation, editing, deletion
│   └── cluster_controller.py  # Diagnostic & /api/health routes
├── views/                     # Jinja2 Templates (Views)
│   ├── base.html              # Base layout with Bootstrap 5.3 & navbar
│   ├── items/
│   │   ├── index.html         # Main dashboard with table, stats cards & modal
│   │   └── edit.html          # Edit item form
│   └── cluster/
│       └── index.html         # PgVisor routing verification panel
└── static/
    └── css/
        └── custom.css         # Styling enhancements for Bootstrap 5
```

---

## Quickstart

### Prerequisites

Start the PgVisor HA cluster first (e.g. from the `examples` directory):

```bash
cd examples
./setup.sh up
```

This starts the PgVisor 3-node cluster and exposes the proxy at `127.0.0.1:5432` and web dashboard at `http://127.0.0.1:8080`.

---

### Option 1: Running with Docker Compose (Together with Cluster)

From the `examples/` directory, launch the entire cluster plus the demo app together:

```bash
cd examples
docker compose -f docker-compose.demo-app.yml up --build
```
Or by combining the overlay:
```bash
docker compose -f docker-compose.yml -f docker-compose.demo-app.yml up -d
```

### Option 2: Running Standalone Demo App Container

If your PgVisor cluster is already running:

```bash
cd examples/demo_app
docker compose up --build
```

The app will start at **http://localhost:5000**.


---

### Option 2: Running Locally with Python Virtualenv

```bash
cd examples/demo_app

# 1. Create and activate a virtual environment
python3 -m venv .venv
source .venv/bin/activate

# 2. Install dependencies
pip install -r requirements.txt

# 3. Configure environment
cp .env.example .env

# 4. Start the application
python app.py
```

Access the application in your browser at **http://127.0.0.1:5000**.

---

## Configuration Variables

| Variable | Default | Description |
| :--- | :--- | :--- |
| `DB_HOST` | `127.0.0.1` (local) / `host.docker.internal` (Docker) | PgVisor proxy hostname |
| `DB_PORT` | `5432` | PgVisor proxy port |
| `DB_NAME` | `postgres` | Target database name |
| `DB_USER` | `postgres` | Database user |
| `DB_PASSWORD` | `postgres` | Database password |
| `DATABASE_URL` | *(unset)* | Full Postgres connection URI (overrides `DB_*`) |
| `PORT` | `5000` | HTTP port for the web application |
| `DEBUG` | `false` | Enable Flask debug mode |
| `SECRET_KEY` | `dev-secret-key-pgvisor-demo` | Flask session secret key |

---

## PgVisor Features Demonstrated

1. **Read/Write Splitting Verification**:
   Navigate to the **Cluster Diagnostic** tab (`/cluster`) and click **Run Read/Write Routing Test**.
   - The read query tests `SELECT pg_is_in_recovery()`, demonstrating that reads are handled by a standby replica.
   - The write query executes a mutating `UPDATE`, demonstrating routing to the primary Raft leader.
2. **Failover Resilience**:
   Because queries go through `pgvisor-proxy`, cluster failovers are masked by connection buffering. If the leader changes, the app's connection pool retries automatically.
3. **Healthcheck Endpoint**:
   `GET /api/health` returns JSON indicating connection health:
   ```json
   {
     "status": "healthy",
     "database": {
       "connected": true,
       "db_name": "postgres",
       "db_user": "postgres",
       "in_recovery": false,
       "node_role": "Leader (Primary)",
       "pg_version": "PostgreSQL 18.0..."
     }
   }
   ```
