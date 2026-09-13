#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor Cluster Bootstrap & Management Script
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMPOSE_FILE="${SCRIPT_DIR}/docker-compose.yml"
ENV_FILE="${SCRIPT_DIR}/.env"
ENV_EXAMPLE="${SCRIPT_DIR}/.env.example"

ACTION="up"
DO_BUILD=false
AUTO_CONFIRM=false
SILENT=false
LOG_ARGS=()

usage() {
    echo "Usage: ./setup.sh [command] [options]"
    echo ""
    echo "Commands:"
    echo "  up (default)    Start the PgVisor HA cluster and wait for healthchecks"
    echo "  down, stop      Stop cluster containers (preserves database data)"
    echo "  restart         Restart cluster containers"
    echo "  status          Show running containers and cluster health"
    echo "  logs [service]  Tail cluster container logs"
    echo "  clean           Stop cluster and delete all persistent database volumes"
    echo ""
    echo "Options:"
    echo "  --build         Rebuild Docker images from source before starting"
    echo "  -y, --yes       Skip confirmation prompt (for clean command)"
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
        -y|--yes)
            AUTO_CONFIRM=true
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
        echo "Stopping PgVisor cluster containers (database volumes preserved)..."
    fi
    docker compose -f "${COMPOSE_FILE}" down
    if [ "${SILENT}" = false ]; then
        echo "PgVisor cluster stopped."
    fi
    exit 0
fi

if [ "${ACTION}" = "clean" ]; then
    if [ "${AUTO_CONFIRM}" != true ]; then
        read -r -p "WARNING: This will permanently delete all PostgreSQL database volumes. Continue? [y/N] " response
        case "${response}" in
            [yY][eE][sS]|[yY])
                ;;
            *)
                echo "Operation cancelled."
                exit 0
                ;;
        esac
    fi
    if [ "${SILENT}" = false ]; then
        echo "Stopping containers and deleting persistent database volumes..."
    fi
    docker compose -f "${COMPOSE_FILE}" down -v --remove-orphans
    if [ "${SILENT}" = false ]; then
        echo "Cluster volumes removed."
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
    echo "Please install Docker: https://docs.docker.com/get-docker/" >&2
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
    # Export non-comment lines
    set -a
    # shellcheck disable=SC1090
    . "${ENV_FILE}"
    set +a
fi

TARGET_IMAGE="${PGVISOR_IMAGE:-dreamoutbox/pgvisor:latest}"

# Determine whether to build image from local source or use existing/pulled image
if [ "${DO_BUILD}" = true ]; then
    if [ "${SILENT}" = false ]; then
        echo "Building PgVisor Docker images from source..."
    fi
    docker compose -f "${COMPOSE_FILE}" build
elif ! docker image inspect "${TARGET_IMAGE}" >/dev/null 2>&1; then
    # If image does not exist locally, check if source Dockerfile is present in parent
    if [ -f "${SCRIPT_DIR}/../Dockerfile" ]; then
        if [ "${SILENT}" = false ]; then
            echo "PgVisor image '${TARGET_IMAGE}' not found locally. Building from local source..."
        fi
        docker compose -f "${COMPOSE_FILE}" build
    else
        if [ "${SILENT}" = false ]; then
            echo "Pulling '${TARGET_IMAGE}' from registry..."
        fi
        docker compose -f "${COMPOSE_FILE}" pull pgvisor-node1 pgvisor-node2 pgvisor-node3 pgvisor-proxy || true
    fi
fi

# Start or restart containers
if [ "${ACTION}" = "restart" ]; then
    if [ "${SILENT}" = false ]; then
        echo "Restarting PgVisor cluster containers..."
    fi
    docker compose -f "${COMPOSE_FILE}" restart
else
    if [ "${SILENT}" = false ]; then
        echo "Starting PgVisor cluster (MinIO + 3 Nodes + 1 Proxy)..."
    fi
    docker compose -f "${COMPOSE_FILE}" up -d
fi

# Wait for healthy services
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
S3_WEB_PORT="${S3_CONSOLE_PORT:-9001}"
ADMIN_TOKEN="${PGVISOR_ADMIN_TOKEN:-postgres}"
DB_USER="${POSTGRES_USER:-postgres}"
DB_PASS="${POSTGRES_PASSWORD:-postgres}"

if [ "${SILENT}" = false ]; then
    echo ""
    echo "========================================================="
    echo "  PgVisor PostgreSQL HA Cluster is Ready!"
    echo "========================================================="
    echo "  - PostgreSQL L7 Proxy:     localhost:${PROXY_PORT}"
    echo "  - Web Management UI:       http://localhost:${DASHBOARD_PORT}"
    echo "  - MinIO S3 Web Console:    http://localhost:${S3_WEB_PORT} (minioadmin / minioadmin)"
    echo ""
    echo "--- How to Connect ---"
    echo "  psql CLI:"
    echo "    psql -h localhost -p ${PROXY_PORT} -U ${DB_USER} -d postgres"
    echo ""
    echo "  Application Connection URI:"
    echo "    postgresql://${DB_USER}:${DB_PASS}@localhost:${PROXY_PORT}/postgres"
    echo ""
    echo "--- Web Dashboard ---"
    echo "  Open http://localhost:${DASHBOARD_PORT} in your browser."
    echo "  Admin Token: ${ADMIN_TOKEN}"
    echo ""
    echo "--- Cluster Management ---"
    echo "  Check cluster status:    ./setup.sh status"
    echo "  View live logs:          ./setup.sh logs"
    echo "  Stop cluster:            ./setup.sh down"
    echo "  Wipe data and reset:     ./setup.sh clean"
    echo "========================================================="
fi
