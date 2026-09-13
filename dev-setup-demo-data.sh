#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Demo Data Setup & Backup Script
#
# Sets up the demo schema/data, creates a full backup ('f1'),
# slowly inserts additional records with a 3-second delay,
# and creates an incremental backup ('incr2').
#
# All output is strictly clean plain text (no ANSI escape codes).
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Configuration with environment variable overrides
PROXY_HOST="${PGVISOR_HOST:-127.0.0.1}"
PROXY_PORT="${PGVISOR_PORT:-5432}"
DASHBOARD_URL="${PGVISOR_DASHBOARD_URL:-http://127.0.0.1:8080}"
ADMIN_TOKEN="${PGVISOR_ADMIN_TOKEN:-postgres}"
NODE_CONTAINER="${PGVISOR_NODE_CONTAINER:-pgvisor-node1}"
PGUSER="${PGUSER:-postgres}"
PGDATABASE="${PGDATABASE:-postgres}"
DELAY_SECONDS="${PGVISOR_DELAY_SECONDS:-3}"

# CLI Arguments
while [[ $# -gt 0 ]]; do
    case "$1" in
        --delay=*)
            DELAY_SECONDS="${1#*=}"
            shift
            ;;
        --host=*)
            PROXY_HOST="${1#*=}"
            shift
            ;;
        --port=*)
            PROXY_PORT="${1#*=}"
            shift
            ;;
        --dashboard-url=*)
            DASHBOARD_URL="${1#*=}"
            shift
            ;;
        --token=*)
            ADMIN_TOKEN="${1#*=}"
            shift
            ;;
        --container=*)
            NODE_CONTAINER="${1#*=}"
            shift
            ;;
        -h|--help)
            echo "Usage: $0 [options]"
            echo ""
            echo "Sets up demo table, seeds data, triggers full backup 'f1',"
            echo "slowly inserts additional rows with delay, and triggers incremental backup 'incr2'."
            echo ""
            echo "Options:"
            echo "  --delay=N            Delay in seconds between slow inserts (default: 3)"
            echo "  --host=HOST          Postgres proxy host (default: 127.0.0.1)"
            echo "  --port=PORT          Postgres proxy port (default: 5432)"
            echo "  --dashboard-url=URL  Dashboard HTTP URL (default: http://127.0.0.1:8080)"
            echo "  --token=TOKEN        Admin token for dashboard API (default: postgres)"
            echo "  --container=NAME     Docker container fallback for psql (default: pgvisor-node1)"
            echo "  -h, --help           Show this help message"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            echo "Run '$0 --help' for usage."
            exit 1
            ;;
    esac
done

# Prepare authentication header for dashboard API
AUTH_HEADER=()
if [ -n "${ADMIN_TOKEN}" ]; then
    AUTH_HEADER=(-H "Authorization: Bearer ${ADMIN_TOKEN}")
fi

# Helper to extract JSON field using jq, python3, or grep/sed
json_extract() {
    local field="$1"
    if command -v jq &> /dev/null; then
        jq -r ".${field} // empty"
    elif command -v python3 &> /dev/null; then
        python3 -c "import sys, json; data = json.load(sys.stdin); print(data.get('${field}', ''))" 2>/dev/null || true
    else
        grep -o "\"${field}\":\"[^\"]*\"" | head -n 1 | cut -d':' -f2 | tr -d '"'
    fi
}

# Helper to execute SQL with retry
run_sql() {
    local query="$1"
    local output=""
    local success=false

    for attempt in 1 2 3; do
        if command -v psql &> /dev/null; then
            if output=$(PGPASSWORD="" psql -h "${PROXY_HOST}" -p "${PROXY_PORT}" -U "${PGUSER}" -d "${PGDATABASE}" -c "${query}" 2>&1); then
                echo "${output}"
                return 0
            fi
        fi

        # Fallback to docker exec if local psql connection fails or is unavailable
        if docker ps --format '{{.Names}}' 2>/dev/null | grep -q "^${NODE_CONTAINER}$"; then
            if output=$(docker exec -i "${NODE_CONTAINER}" psql -U "${PGUSER}" -d "${PGDATABASE}" -c "${query}" 2>&1); then
                echo "${output}"
                return 0
            fi
        fi
        sleep 1
    done

    echo "SQL Execution Failed: ${output}" >&2
    return 1
}

# Helper to trigger a backup snapshot via Dashboard API
trigger_backup() {
    local b_type="$1"
    local b_label="$2"

    local resp
    resp=$(curl -s -f -X POST "${DASHBOARD_URL}/api/backups" \
        "${AUTH_HEADER[@]}" \
        -H "Content-Type: application/json" \
        -d "{\"backup_type\": \"${b_type}\", \"label\": \"${b_label}\"}")

    echo "${resp}"
}

# Helper to flush and rotate WAL segment so changes are archived immediately
switch_wal() {
    run_sql "SELECT pg_switch_wal();" > /dev/null 2>&1 || true
}

echo "============================================================="
echo "  PgVisor: Demo Data Setup & Backup Sequence"
echo "  Proxy: ${PROXY_HOST}:${PROXY_PORT} | Dashboard: ${DASHBOARD_URL}"
echo "============================================================="

# ------------------------------------------------------------------------------
# Step 1: Initialize demo table and seed initial rows
# ------------------------------------------------------------------------------
echo ""
echo "[Step 1/4] Setting up demo table 'pgvisor_demo' and seeding initial rows..."

SCHEMA_AND_SEED_SQL="
DROP TABLE IF EXISTS pgvisor_demo;

CREATE TABLE pgvisor_demo (
    id SERIAL PRIMARY KEY,
    name VARCHAR(100) NOT NULL,
    status VARCHAR(50) DEFAULT 'active',
    counter INT DEFAULT 0,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

INSERT INTO pgvisor_demo (name, status, counter) VALUES
    ('alpha', 'active', 10),
    ('beta', 'active', 20),
    ('gamma', 'pending', 30),
    ('delta', 'archived', 40);
"

run_sql "${SCHEMA_AND_SEED_SQL}"
echo "Initial data seeded successfully (4 rows: alpha, beta, gamma, delta)."
switch_wal

# ------------------------------------------------------------------------------
# Step 2: Trigger full backup 'f1'
# ------------------------------------------------------------------------------
echo ""
echo "[Step 2/4] Triggering full backup 'f1' via Dashboard API..."
F1_RESP=$(trigger_backup "full" "f1")
F1_SNAPSHOT_ID=$(echo "${F1_RESP}" | json_extract "snapshot_id")
F1_CREATED_AT=$(echo "${F1_RESP}" | json_extract "created_at")

if [ -z "${F1_SNAPSHOT_ID}" ]; then
    echo "Error: Failed to create full backup 'f1'. API Response: ${F1_RESP}" >&2
    exit 1
fi

echo "Full backup 'f1' completed successfully:"
echo "  Snapshot ID : ${F1_SNAPSHOT_ID}"
echo "  Created At  : ${F1_CREATED_AT}"
echo "  Label       : f1"
echo "  Type        : full"

# ------------------------------------------------------------------------------
# Step 3: Slowly execute inserts with delay
# ------------------------------------------------------------------------------
echo ""
echo "[Step 3/4] Slowly executing incremental insert statements (delay: ${DELAY_SECONDS}s)..."

INSERTS=(
    "INSERT INTO pgvisor_demo (name, status, counter) VALUES ('echo', 'active', 50);"
    "INSERT INTO pgvisor_demo (name, status, counter) VALUES ('foxtrot', 'pending', 60);"
    "INSERT INTO pgvisor_demo (name, status, counter) VALUES ('golf', 'archived', 70);"
)

NAMES=("echo" "foxtrot" "golf")
TIMESTAMPS=()

for i in "${!INSERTS[@]}"; do
    NAME="${NAMES[$i]}"
    QUERY="${INSERTS[$i]}"
    NOW_UTC=$(date -u +"%Y-%m-%d %H:%M:%S%z")
    NOW_ISO=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
    TIMESTAMPS+=("${NAME}: ${NOW_ISO}")

    echo ""
    echo "  -> [${NOW_UTC}] Inserting '${NAME}'..."
    run_sql "${QUERY}"
    echo "     Inserted '${NAME}' successfully."

    # Delay between inserts
    if [ "$i" -lt $((${#INSERTS[@]} - 1)) ]; then
        echo "     Waiting ${DELAY_SECONDS} seconds before next insert..."
        sleep "${DELAY_SECONDS}"
    fi
done

# Small buffer and WAL switch to ensure all WAL records are flushed to archive
sleep 1
switch_wal

# ------------------------------------------------------------------------------
# Step 4: Trigger incremental backup 'incr2'
# ------------------------------------------------------------------------------
echo ""
echo "[Step 4/4] Triggering incremental backup 'incr2' via Dashboard API..."
INCR2_RESP=$(trigger_backup "incremental" "incr2")
INCR2_SNAPSHOT_ID=$(echo "${INCR2_RESP}" | json_extract "snapshot_id")
INCR2_CREATED_AT=$(echo "${INCR2_RESP}" | json_extract "created_at")

if [ -z "${INCR2_SNAPSHOT_ID}" ]; then
    echo "Error: Failed to create incremental backup 'incr2'. API Response: ${INCR2_RESP}" >&2
    exit 1
fi

echo "Incremental backup 'incr2' completed successfully:"
echo "  Snapshot ID : ${INCR2_SNAPSHOT_ID}"
echo "  Created At  : ${INCR2_CREATED_AT}"
echo "  Label       : incr2"
echo "  Type        : incremental"

# ------------------------------------------------------------------------------
# Verification: Display current table rows and PITR timeline summary
# ------------------------------------------------------------------------------
echo ""
echo "============================================================="
echo "  Current 'pgvisor_demo' Table State"
echo "============================================================="
run_sql "SELECT id, name, status, counter, created_at FROM pgvisor_demo ORDER BY id;"

echo ""
echo "============================================================="
echo "  Demo Setup Summary & PITR Reference Points"
echo "============================================================="
echo "1. Full backup 'f1'          : ${F1_SNAPSHOT_ID} (${F1_CREATED_AT})"
echo "2. Slow inserts timeline     :"
for ts in "${TIMESTAMPS[@]}"; do
    echo "   - ${ts}"
done
echo "3. Incremental backup 'incr2': ${INCR2_SNAPSHOT_ID} (${INCR2_CREATED_AT})"
echo ""
echo "To test Point-In-Time-Recovery (PITR) to a specific point:"
echo "  curl -X POST \"${DASHBOARD_URL}/api/backups/${INCR2_SNAPSHOT_ID}/restore\" \\"
echo "    -H \"Authorization: Bearer ${ADMIN_TOKEN}\" \\"
echo "    -H \"Content-Type: application/json\" \\"
echo "    -d '{\"recovery_target_time\": \"<ISO-8601-TIMESTAMP>\"}'"
echo "============================================================="
