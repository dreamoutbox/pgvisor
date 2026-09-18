#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor HA Cluster + Python MVC Demo App Launcher
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMPOSE_FILE="${SCRIPT_DIR}/docker-compose.demo-app.yml"
ENV_FILE="${SCRIPT_DIR}/.env"
ENV_EXAMPLE="${SCRIPT_DIR}/.env.example"

ACTION="up"
DO_BUILD=false
SILENT=false
LOG_ARGS=()

usage() {
    echo "Usage: ./run-demo-app.sh [command] [options]"
    echo ""
    echo "Commands:"
    echo "  up (default)    Start PgVisor cluster and Python MVC demo web app"
    echo "  down, stop      Stop all cluster and demo app containers"
    echo "  restart         Restart cluster and demo app containers"
    echo "  status          Show running containers status"
    echo "  logs [service]  Tail logs from containers (e.g. ./run-demo-app.sh logs demo-app)"
    echo "  clean           Stop containers and delete all persistent database volumes"
    echo ""
    echo "Options:"
    echo "  --build         Rebuild Docker images before starting"
    echo "  -s, --silent    Minimize output"
    echo "  -h, --help      Show this help message"
}

# Parse command line arguments
while [ $# -gt 0 ]; do
    case "$1" in
        up|down|stop|restart|status|clean)
            ACTION="$1"
            shift
            ;;
        logs)
            ACTION="logs"
            shift
            while [ $# -gt 0 ]; do
                LOG_ARGS+=("$1")
                shift
            done
            break
            ;;
        --build)
            DO_BUILD=true
            shift
            ;;
        -s|--silent)
            SILENT=true
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "ERROR: Unknown option '$1'" >&2
            echo "" >&2
            usage >&2
            exit 1
            ;;
    esac
done

# Execute commands that don't start the cluster
if [ "${ACTION}" = "down" ] || [ "${ACTION}" = "stop" ]; then
    if [ "${SILENT}" = false ]; then
        echo "Stopping PgVisor cluster and Demo App containers..."
    fi
    docker compose -f "${COMPOSE_FILE}" down
    if [ "${SILENT}" = false ]; then
        echo "Containers stopped."
    fi
    exit 0
fi

if [ "${ACTION}" = "clean" ]; then
    if [ "${SILENT}" = false ]; then
        echo "Stopping containers and deleting persistent database volumes..."
    fi
    docker compose -f "${COMPOSE_FILE}" down -v --remove-orphans
    if [ "${SILENT}" = false ]; then
        echo "All persistent volumes removed."
    fi
    exit 0
fi

if [ "${ACTION}" = "status" ]; then
    docker compose -f "${COMPOSE_FILE}" ps
    exit 0
fi

if [ "${ACTION}" = "logs" ]; then
    if [ ${#LOG_ARGS[@]} -gt 0 ]; then
        docker compose -f "${COMPOSE_FILE}" logs -f "${LOG_ARGS[@]}"
    else
        docker compose -f "${COMPOSE_FILE}" logs -f
    fi
    exit 0
fi

# Pre-flight environment and dependency checks
if ! command -v docker >/dev/null 2>&1; then
    echo "ERROR: 'docker' is required but not installed or not in PATH." >&2
    exit 1
fi

if ! docker compose version >/dev/null 2>&1; then
    echo "ERROR: 'docker compose' (v2+) is required but not available." >&2
    exit 1
fi

# Ensure .env exists, copying from .env.example if present
if [ ! -f "${ENV_FILE}" ]; then
    if [ -f "${ENV_EXAMPLE}" ]; then
        cp "${ENV_EXAMPLE}" "${ENV_FILE}"
        if [ "${SILENT}" = false ]; then
            echo "Created default configuration file: ${ENV_FILE}"
        fi
    fi
fi

# Load environment variables from .env if present
if [ -f "${ENV_FILE}" ]; then
    set -a
    # shellcheck disable=SC1090
    . "${ENV_FILE}"
    set +a
fi

# Start or restart containers
BUILD_FLAG=()
if [ "${DO_BUILD}" = true ]; then
    BUILD_FLAG=(--build)
fi

if [ "${ACTION}" = "restart" ]; then
    if [ "${SILENT}" = false ]; then
        echo "Restarting PgVisor cluster and Demo App containers..."
    fi
    docker compose -f "${COMPOSE_FILE}" restart
else
    if [ "${SILENT}" = false ]; then
        echo "Starting PgVisor HA cluster and Python Demo App..."
    fi
    docker compose -f "${COMPOSE_FILE}" up -d "${BUILD_FLAG[@]}"
fi

# Wait for healthy cluster services
HEALTH_TIMEOUT=90
DEADLINE=$(( $(date +%s) + HEALTH_TIMEOUT ))
REQUIRED_CONTAINERS=("pgvisor-minio" "pgvisor-node1" "pgvisor-node2" "pgvisor-node3")

if [ "${SILENT}" = false ]; then
    echo "Waiting for cluster services to become healthy..."
fi

while [ "$(date +%s)" -lt "${DEADLINE}" ]; do
    ALL_HEALTHY=true
    for c in "${REQUIRED_CONTAINERS[@]}"; do
        status=$(docker inspect -f '{{.State.Status}}' "$c" 2>/dev/null || echo "missing")
        if [ "$status" = "exited" ] || [ "$status" = "dead" ]; then
            echo "ERROR: Container '$c' exited unexpectedly!" >&2
            docker logs --tail 30 "$c" >&2 || true
            exit 1
        fi
        health=$(docker inspect -f '{{.State.Health.Status}}' "$c" 2>/dev/null || echo "none")
        if [ "$health" != "healthy" ]; then
            ALL_HEALTHY=false
            break
        fi
    done

    if [ "$ALL_HEALTHY" = true ]; then
        break
    fi
    sleep 1
done

if [ "$ALL_HEALTHY" != true ]; then
    echo "ERROR: Timed out waiting for cluster containers to report healthy after ${HEALTH_TIMEOUT}s:" >&2
    for c in "${REQUIRED_CONTAINERS[@]}"; do
        echo "  - $c: $(docker inspect -f 'status={{.State.Status}}, health={{.State.Health.Status}}' "$c" 2>/dev/null || echo 'not found')" >&2
    done
    exit 1
fi

PROXY_PORT="${PGVISOR_PORT:-5432}"
DASHBOARD_PORT="${PGVISOR_DASHBOARD_PORT:-8080}"
DEMO_PORT="${DEMO_APP_PORT:-5000}"
S3_WEB_PORT="${S3_CONSOLE_PORT:-9001}"
ADMIN_TOKEN="${PGVISOR_ADMIN_TOKEN:-postgres}"
DB_USER="${POSTGRES_USER:-postgres}"

if [ "${SILENT}" = false ]; then
    echo ""
    echo "========================================================="
    echo "  PgVisor Cluster & Python Demo App are Ready!"
    echo "========================================================="
    echo "  - Python MVC Demo Web App: http://localhost:${DEMO_PORT}"
    echo "  - Web Management UI:       http://localhost:${DASHBOARD_PORT}"
    echo "  - PostgreSQL L7 Proxy:     localhost:${PROXY_PORT}"
    echo "  - MinIO S3 Console:        http://localhost:${S3_WEB_PORT}"
    echo ""
    echo "--- Useful Commands ---"
    echo "  View demo app logs:      ./run-demo-app.sh logs demo-app"
    echo "  Check container status:  ./run-demo-app.sh status"
    echo "  Stop everything:         ./run-demo-app.sh down"
    echo "  Wipe volumes and reset:  ./run-demo-app.sh clean"
    echo "========================================================="
fi
