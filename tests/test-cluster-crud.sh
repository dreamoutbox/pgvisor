#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Demo CRUD Test (Self-Contained Concurrent Test Profile)
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
SQL_FILE="${REPO_ROOT}/scripts/test-crud.sql"

# Pre-defined test port & project constants
readonly TEST_PROXY_PORT=5532
readonly TEST_DASHBOARD_PORT=8180
readonly TEST_MINIO_PORT=9100
readonly TEST_MINIO_CONSOLE=9101
readonly PROJECT_NAME="pgvisor-crud"
readonly COMPOSE_FILE="${REPO_ROOT}/composes/docker-compose.crud.yml"

if [ ! -f "${SQL_FILE}" ]; then
    echo "Error: SQL file not found at ${SQL_FILE}"
    exit 1
fi

cleanup() {
    echo "Tearing down cluster ${PROJECT_NAME}..."
    docker compose -p "${PROJECT_NAME}" -f "${COMPOSE_FILE}" down -v --remove-orphans > /dev/null 2>&1 || true
}
trap cleanup EXIT

echo "========================================================="
echo "  Executing PgVisor Demo CRUD Test (Port: ${TEST_PROXY_PORT})"
echo "========================================================="

echo "[1/3] Starting isolated test cluster ${PROJECT_NAME}..."
docker compose -p "${PROJECT_NAME}" -f "${COMPOSE_FILE}" down -v --remove-orphans > /dev/null 2>&1 || true
docker compose -p "${PROJECT_NAME}" -f "${COMPOSE_FILE}" up -d

echo "[2/3] Waiting for cluster nodes to report healthy..."
until [ "$(docker inspect -f '{{.State.Health.Status}}' "pgvisor-crud-node1" 2>/dev/null)" = "healthy" ] && \
      [ "$(docker inspect -f '{{.State.Health.Status}}' "pgvisor-crud-node2" 2>/dev/null)" = "healthy" ] && \
      [ "$(docker inspect -f '{{.State.Health.Status}}' "pgvisor-crud-node3" 2>/dev/null)" = "healthy" ]; do
    sleep 1
done

echo "[3/3] Executing CRUD operations..."
if command -v psql &> /dev/null; then
    echo "Using local psql connecting to PgVisor proxy at localhost:${TEST_PROXY_PORT}..."
    PGPASSWORD="" psql -h localhost -p "${TEST_PROXY_PORT}" -U postgres -d postgres -f "${SQL_FILE}"
else
    echo "Running psql inside pgvisor-crud-proxy container..."
    docker compose -p "${PROJECT_NAME}" -f "${COMPOSE_FILE}" exec -T pgvisor-proxy psql -h localhost -p 5432 -U postgres -d postgres -f /scripts/test-crud.sql
fi

echo ""
echo "Demo CRUD test finished successfully!"
