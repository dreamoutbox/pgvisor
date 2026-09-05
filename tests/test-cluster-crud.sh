#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
SQL_FILE="${REPO_ROOT}/scripts/test-crud.sql"

if [ ! -f "${SQL_FILE}" ]; then
    echo "Error: SQL file not found at ${SQL_FILE}"
    exit 1
fi

echo "========================================================="
echo "  Executing PgVisor Demo CRUD Test"
echo "========================================================="

if command -v psql &> /dev/null; then
    echo "Using local psql client connecting to PgVisor proxy at localhost:5432..."
    PGPASSWORD="" psql -h localhost -p 5432 -U postgres -d postgres -f "${SQL_FILE}"
else
    echo "Local psql not found; running psql inside pgvisor-proxy container..."
    docker compose exec -T pgvisor-proxy psql -h localhost -p 5432 -U postgres -d postgres -f /scripts/test-crud.sql
fi

echo ""
echo "Demo CRUD test finished successfully!"
