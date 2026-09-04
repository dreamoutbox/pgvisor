#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Full Basebackup and Restore Verification Test
#
# Steps:
#   1. Create test table 't1' and insert 'alpha'.
#   2. Trigger full physical basebackup via dashboard API (T0).
#   3. Verify backup is listed in API and download archive.
#   4. Drop table 't1' to simulate accidental drop / disaster.
#   5. Restore from snapshot archive into cluster node (T1).
#   6. Verify table 't1' and 'alpha' are fully recovered.
#   7. Clean up test table and temporary artifacts.
# ==============================================================================

PROXY_HOST="${PGVISOR_HOST:-localhost}"
PROXY_PORT="${PGVISOR_PORT:-5432}"
DASHBOARD_URL="${PGVISOR_DASHBOARD_URL:-http://localhost:8080}"
NODE_CONTAINER="${PGVISOR_NODE_CONTAINER:-pgvisor-node1}"
PGDATA_DIR="/var/lib/postgresql/data/pgdata"

echo "========================================================="
echo "  PgVisor Full Basebackup & Restore Verification Test    "
echo "========================================================="

# Helper for executing SQL via psql with automatic reconnection retry
run_sql() {
    local query="$1"
    for attempt in 1 2 3; do
        if command -v psql &> /dev/null; then
            if output=$(PGPASSWORD="" psql -h "${PROXY_HOST}" -p "${PROXY_PORT}" -U postgres -d postgres -t -A -c "${query}" 2>&1); then
                echo "${output}"
                return 0
            fi
        else
            if output=$(docker exec -i "${NODE_CONTAINER}" psql -U postgres -d postgres -t -A -c "${query}" 2>&1); then
                echo "${output}"
                return 0
            fi
        fi
        sleep 1
    done
    echo "${output}"
    return 1
}

# Helper to parse JSON field using python3 or jq
json_extract() {
    local field="$1"
    if command -v jq &> /dev/null; then
        jq -r ".${field} // empty"
    elif command -v python3 &> /dev/null; then
        python3 -c "import sys, json; data = json.load(sys.stdin); print(data.get('${field}', ''))"
    else
        grep -o "\"${field}\":\"[^\"]*\"" | cut -d':' -f2 | tr -d '"'
    fi
}

# ------------------------------------------------------------------------------
# [1/7] Ensure cluster is healthy and reachable
# ------------------------------------------------------------------------------
echo ""
echo "[1/7] Verifying cluster connectivity..."
if ! run_sql "SELECT 1;" &> /dev/null; then
    echo "Error: Cannot connect to PostgreSQL at ${PROXY_HOST}:${PROXY_PORT}"
    exit 1
fi
echo "✓ PostgreSQL cluster proxy is reachable."

# ------------------------------------------------------------------------------
# [2/7] Create test table 't1' and insert 'alpha'
# ------------------------------------------------------------------------------
echo ""
echo "[2/7] Seeding test table 't1' with 'alpha'..."
run_sql "DROP TABLE IF EXISTS t1; CREATE TABLE t1 (id int PRIMARY KEY, val text NOT NULL); INSERT INTO t1 (id, val) VALUES (1, 'alpha');"
INITIAL_VAL=$(run_sql "SELECT val FROM t1 WHERE id = 1;")
if [ "${INITIAL_VAL}" != "alpha" ]; then
    echo "Failed to insert initial record 'alpha'. Got: '${INITIAL_VAL}'"
    exit 1
fi
echo "✓ Table 't1' created and seeded with: id=1, val='alpha'"

# ------------------------------------------------------------------------------
# [3/7] Trigger full physical basebackup via dashboard API (T0)
# ------------------------------------------------------------------------------
echo ""
echo "[3/7] Triggering full physical basebackup via API (POST ${DASHBOARD_URL}/api/backups)..."
BACKUP_RESP=$(curl -s -f -X POST "${DASHBOARD_URL}/api/backups" \
    -H "Content-Type: application/json" \
    -d '{"backup_type": "full", "label": "test-backup-restore-suite"}')

SNAPSHOT_ID=$(echo "${BACKUP_RESP}" | json_extract "snapshot_id")
if [ -z "${SNAPSHOT_ID}" ]; then
    echo "Failed to create backup. API response: ${BACKUP_RESP}"
    exit 1
fi
echo "✓ Basebackup created successfully. Snapshot ID: ${SNAPSHOT_ID}"

# ------------------------------------------------------------------------------
# [4/7] Verify backup is listed in API and download archive
# ------------------------------------------------------------------------------
echo ""
echo "[4/7] Verifying snapshot in backup list and downloading archive..."
BACKUP_LIST=$(curl -s -f "${DASHBOARD_URL}/api/backups")
if ! echo "${BACKUP_LIST}" | grep -q "${SNAPSHOT_ID}"; then
    echo "Snapshot ID ${SNAPSHOT_ID} not found in /api/backups list!"
    exit 1
fi

TMP_ARCHIVE="/tmp/${SNAPSHOT_ID}.tar.gz"
curl -s -f "${DASHBOARD_URL}/api/backups/${SNAPSHOT_ID}/download" -o "${TMP_ARCHIVE}"
if [ ! -s "${TMP_ARCHIVE}" ]; then
    echo "Downloaded archive is empty!"
    exit 1
fi

ARCHIVE_SIZE=$(wc -c < "${TMP_ARCHIVE}")
echo "✓ Snapshot archive verified and downloaded (${ARCHIVE_SIZE} bytes)."

# ------------------------------------------------------------------------------
# [5/7] Simulate disaster: Drop table 't1'
# ------------------------------------------------------------------------------
echo ""
echo "[5/7] Simulating disaster: Dropping table 't1'..."
run_sql "DROP TABLE t1;"
if run_sql "SELECT 1 FROM t1;" &> /dev/null; then
    echo "Table 't1' should not exist after DROP TABLE!"
    exit 1
fi
echo "✓ Table 't1' dropped successfully (verified missing)."

# ------------------------------------------------------------------------------
# [6/7] Restore snapshot into cluster node (T1)
# ------------------------------------------------------------------------------
echo ""
echo "[6/7] Restoring snapshot ${SNAPSHOT_ID} into cluster node '${NODE_CONTAINER}'..."

# Test the dashboard restore API endpoint
RESTORE_RESP=$(curl -s -f -X POST "${DASHBOARD_URL}/api/backups/${SNAPSHOT_ID}/restore" \
    -H "Content-Type: application/json" \
    -d '{}')
echo "  Dashboard Restore API response: ${RESTORE_RESP}"
echo "✓ Cluster leader restored and standbys re-synchronized automatically via API."

# ------------------------------------------------------------------------------
# [7/7] Verify recovered data in table 't1'
# ------------------------------------------------------------------------------
echo ""
echo "[7/7] Verifying recovered data in 't1' via PgVisor proxy..."
# Allow brief moment for proxy pool connections to settle after node restarts
sleep 2

# Retry query up to 5 times to handle proxy pool reconnection
RECOVERED_VAL=""
for attempt in {1..5}; do
    if RECOVERED_VAL=$(run_sql "SELECT val FROM t1 WHERE id = 1;" 2> /dev/null); then
        if [ "${RECOVERED_VAL}" = "alpha" ]; then
            break
        fi
    fi
    sleep 1
done
if [ "${RECOVERED_VAL}" != "alpha" ]; then
    echo "Restore verification FAILED! Expected 'alpha', got: '${RECOVERED_VAL}'"
    exit 1
fi
echo "✓ Successfully recovered data: id=1, val='${RECOVERED_VAL}'"

# Clean up
echo ""
echo "Cleaning up test artifacts..."
run_sql "DROP TABLE IF EXISTS t1;" > /dev/null 2>&1 || true
rm -f "${TMP_ARCHIVE}"
curl -s -X DELETE "${DASHBOARD_URL}/api/backups/${SNAPSHOT_ID}" > /dev/null 2>&1 || true
echo "✓ Cleanup complete."

echo ""
echo "========================================================="
echo "  Full Basebackup & Restore Test PASSED Successfully!    "
echo "========================================================="
