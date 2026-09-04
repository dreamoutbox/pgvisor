#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SQL_FILE="${SCRIPT_DIR}/scripts/test-crud.sql"

echo "========================================================="
echo "  Executing PgVisor Demo CRUD Test"
echo "========================================================="

if command -v psql &> /dev/null; then
    echo "Using local psql client connecting to PgVisor proxy at localhost:5432..."
    PGPASSWORD="" psql -h localhost -p 5432 -U postgres -d postgres -f "${SQL_FILE}"
else
    echo "Local psql not found; running psql inside pgvisor-proxy container..."
    docker compose exec pgvisor-proxy psql -h pgvisor-node1 -p 5432 -U postgres -d postgres -f /scripts/test-crud.sql
fi

echo ""
echo "Demo CRUD test finished successfully!"
