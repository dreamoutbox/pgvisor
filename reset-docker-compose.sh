#!/usr/bin/env bash
set -euo pipefail

DO_BUILD=true
SILENT=false

for arg in "$@"; do
    case "$arg" in
        --no-build)
            DO_BUILD=false
            ;;
        -s|--silent|-q|--quiet)
            SILENT=true
            ;;
        -h|--help)
            echo "Usage: $0 [options]"
            echo ""
            echo "Options:"
            echo "  --no-build         Skip Docker image build (default is to always build)"
            echo "  -s, --silent       Run silently unless an error occurs"
            echo "  -h, --help         Show this help message"
            exit 0
            ;;
    esac
done

# Temporary log file for capturing command output in silent mode
ERR_LOG=$(mktemp /tmp/reset-docker-compose.XXXXXX.log)
cleanup_log() {
    rm -f "${ERR_LOG}"
}
trap cleanup_log EXIT

# Helper to run a command; if silent, capture output and dump on error
run_cmd() {
    if [ "${SILENT}" = true ]; then
        if ! "$@" > "${ERR_LOG}" 2>&1; then
            cat "${ERR_LOG}" >&2
            return 1
        fi
    else
        "$@"
    fi
}

log_msg() {
    if [ "${SILENT}" = false ]; then
        echo "$@"
    fi
}

log_msg "========================================================="
log_msg "  PgVisor Cluster Reset & Build Script"
log_msg "========================================================="

log_msg "[1/4] Stopping containers and removing persistent volumes..."
run_cmd docker compose down -v --remove-orphans

if [ "$DO_BUILD" = true ]; then
    log_msg "[2/4] Building PgVisor Docker images (default)..."
    run_cmd docker build -t pgvisor-test-node:latest -t pgvisor-test-proxy:latest .
    run_cmd docker compose build
else
    log_msg "[2/4] Skipping Docker image build (--no-build specified)..."
fi

log_msg "[3/4] Launching MinIO + 3 Nodes + 1 Proxy services..."
run_cmd docker compose up -d

# Wait for minio-init to complete and clean up the dangling container
if docker inspect pgvisor-minio-init > /dev/null 2>&1; then
    log_msg "Waiting for minio-init to complete and cleaning up container..."
    exit_code=$(timeout 30 docker wait pgvisor-minio-init 2>/dev/null || echo "timeout")
    if [ "${exit_code}" != "0" ]; then
        echo "ERROR: pgvisor-minio-init failed with exit code ${exit_code}" >&2
        docker logs pgvisor-minio-init >&2 || true
        exit 1
    fi
    docker rm -f pgvisor-minio-init > /dev/null 2>&1 || true
fi

log_msg "[4/4] Verifying cluster startup and waiting for health checks..."
HEALTH_TIMEOUT=120
DEADLINE=$(( $(date +%s) + HEALTH_TIMEOUT ))
REQUIRED_CONTAINERS=("pgvisor-minio" "pgvisor-node1" "pgvisor-node2" "pgvisor-node3")

while [ "$(date +%s)" -lt "${DEADLINE}" ]; do
    ALL_HEALTHY=true
    for c in "${REQUIRED_CONTAINERS[@]}"; do
        status=$(docker inspect -f '{{.State.Status}}' "$c" 2>/dev/null || echo "missing")
        if [ "$status" = "exited" ] || [ "$status" = "dead" ]; then
            echo "ERROR: Container $c exited unexpectedly!" >&2
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

    if [ "${SILENT}" = false ]; then
        echo "Waiting for PostgreSQL cluster nodes to report healthy..."
    fi
    sleep 1
done

if [ "$ALL_HEALTHY" != true ]; then
    echo "ERROR: Timed out waiting for containers to become healthy after ${HEALTH_TIMEOUT}s:" >&2
    for c in "${REQUIRED_CONTAINERS[@]}"; do
        echo "  - $c: $(docker inspect -f 'status={{.State.Status}}, health={{.State.Health.Status}}' "$c" 2>/dev/null || echo 'not found')" >&2
    done
    exit 1
fi

if [ "${SILENT}" = false ]; then
    echo "========================================================="
    echo "  PgVisor Cluster is Ready!"
    echo "========================================================="
    echo "  - PostgreSQL L7 Proxy: localhost:5432"
    echo "  - Web Dashboard:       http://localhost:8080"
    echo "  - MinIO S3 API:        http://localhost:9000"
    echo "  - MinIO Web Console:   http://localhost:9001 (minioadmin:minioadmin)"
    echo ""
    echo "To test PostgreSQL cluster operations, run:"
    echo "  ./tests/test-cluster-crud.sh"
    echo "========================================================="
fi
