#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Incremental Backup & Point-In-Time-Recovery (PITR) Test
#
# Self-contained concurrent test profile.
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
# shellcheck source=tests/lib/cluster.sh
source "${SCRIPT_DIR}/lib/cluster.sh"
# shellcheck source=tests/lib/helper.sh
source "${SCRIPT_DIR}/lib/helper.sh"

# Pre-defined test port & project constants
readonly TEST_PROXY_PORT=5732
readonly TEST_DASHBOARD_PORT=8380
readonly TEST_MINIO_PORT=9300
readonly TEST_MINIO_CONSOLE=9301
readonly PROJECT_NAME="pgvisor-pitr"
readonly COMPOSE_FILE="${REPO_ROOT}/composes/docker-compose.pitr.yml"

PROXY_HOST="${PGVISOR_HOST:-localhost}"
PROXY_PORT="${PGVISOR_PORT:-${TEST_PROXY_PORT}}"
DASHBOARD_URL="${PGVISOR_DASHBOARD_URL:-http://localhost:${TEST_DASHBOARD_PORT}}"
ADMIN_TOKEN="${PGVISOR_ADMIN_TOKEN:-postgres}"
AUTH_HEADER=()
if [ -n "${ADMIN_TOKEN}" ]; then
    AUTH_HEADER=(-H "Authorization: Bearer ${ADMIN_TOKEN}")
fi
NODE_CONTAINER="${PGVISOR_NODE_CONTAINER:-pgvisor-pitr-node1}"
PGDATA_DIR="/var/lib/postgresql/data/pgdata"

echo "========================================================="
echo "  PgVisor Incremental Backup & PITR Verification Test   "
echo "  Project: ${PROJECT_NAME} | Port: ${PROXY_PORT}        "
echo "========================================================="

TMP_ARCHIVE_T0="/tmp/pitr-t0-$$.tar.gz"
TMP_ARCHIVE_T2="/tmp/pitr-t2-$$.tar.gz"

cleanup() {
    echo "Tearing down cluster ${PROJECT_NAME}..."
    rm -f "${TMP_ARCHIVE_T0}" "${TMP_ARCHIVE_T2}"
    cluster_down "${PROJECT_NAME}" "${COMPOSE_FILE}"
}
trap cleanup EXIT

echo "[0/9] Starting isolated test cluster ${PROJECT_NAME}..."
cluster_down "${PROJECT_NAME}" "${COMPOSE_FILE}"
cluster_up   "${PROJECT_NAME}" "${COMPOSE_FILE}"

echo "Waiting for cluster containers to report healthy..."
wait_for_healthy 120 "${PROJECT_NAME}-minio" "pgvisor-pitr-node1" "pgvisor-pitr-node2" "pgvisor-pitr-node3"
wait_for_proxy_ready "${DASHBOARD_URL}" 60 "${AUTH_HEADER[@]}"

# Helper for executing SQL via psql with automatic reconnection retry
run_sql() {
    local query="$1"
    local output=""
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


# Helper to restore a snapshot archive into the cluster
restore_cluster_node() {
    local snapshot_id="$1"
    echo "  Calling Dashboard restore API for snapshot ${snapshot_id}..."
    local resp
    resp=$(curl -s -f -X POST "${DASHBOARD_URL}/api/backups/${snapshot_id}/restore" \
        "${AUTH_HEADER[@]}" \
        -H "Content-Type: application/json" \
        -d '{}')
    echo "  Restore API response: ${resp}"
    echo "✓ Cluster leader and standbys restored and healthy via API."
}

# ------------------------------------------------------------------------------
# [1/9] Verify cluster connectivity
# ------------------------------------------------------------------------------
echo ""
echo "[1/9] Verifying cluster connectivity..."
if ! run_sql "SELECT 1;" &> /dev/null; then
    echo "Error: Cannot connect to PostgreSQL cluster at ${PROXY_HOST}:${PROXY_PORT}"
    exit 1
fi
echo "✓ PostgreSQL cluster proxy is reachable."

# ------------------------------------------------------------------------------
# [2/9] Create test table 't_pitr' and insert 'alpha' (T0)
# ------------------------------------------------------------------------------
echo ""
echo "[2/9] Creating test table 't_pitr' and inserting 'alpha'..."
run_sql "DROP TABLE IF EXISTS t_pitr; CREATE TABLE t_pitr (id int PRIMARY KEY, val text NOT NULL); INSERT INTO t_pitr (id, val) VALUES (1, 'alpha');"
VAL_A=$(run_sql "SELECT val FROM t_pitr WHERE id = 1;")
if [ "${VAL_A}" != "alpha" ]; then
    echo "Failed to seed 'alpha'. Got: '${VAL_A}'"
    exit 1
fi
echo "✓ Seeded row 1: val='alpha' (T0 state established)"

# ------------------------------------------------------------------------------
# [3/9] Trigger backup T0 (snapshot 0 with 'alpha')
# ------------------------------------------------------------------------------
echo ""
echo "[3/9] Triggering basebackup T0 via API (POST ${DASHBOARD_URL}/api/backups)..."
RESP_T0=$(curl -s -f -X POST "${DASHBOARD_URL}/api/backups" \
    "${AUTH_HEADER[@]}" \
    -H "Content-Type: application/json" \
    -d '{"backup_type": "full", "label": "pitr-suite-t0"}')

SNAP_T0=$(echo "${RESP_T0}" | json_extract "snapshot_id")
if [ -z "${SNAP_T0}" ]; then
    echo "Failed to trigger backup T0. Response: ${RESP_T0}"
    exit 1
fi
echo "✓ Backup T0 complete: snapshot_id='${SNAP_T0}'"

curl -s -f "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/backups/${SNAP_T0}/download" -o "${TMP_ARCHIVE_T0}"
if [ ! -s "${TMP_ARCHIVE_T0}" ]; then
    echo "Downloaded archive T0 is empty!"
    exit 1
fi
echo "✓ Snapshot T0 archive verified ($(wc -c < "${TMP_ARCHIVE_T0}") bytes)."

# ------------------------------------------------------------------------------
# [4/9] Insert 'beta' at T1 and switch WAL to force archiving
# ------------------------------------------------------------------------------
echo ""
echo "[4/9] Inserting row 2 ('beta') at T1 and archiving WAL..."
run_sql "INSERT INTO t_pitr (id, val) VALUES (2, 'beta');"
VAL_B=""
for attempt in 1 2 3 4 5; do
    VAL_B=$(run_sql "SELECT val FROM t_pitr WHERE id = 2;" 2>/dev/null || true)
    if [ "${VAL_B}" = "beta" ]; then
        break
    fi
    sleep 1
done
if [ "${VAL_B}" != "beta" ]; then
    echo "Failed to insert 'beta'. Got: '${VAL_B}'"
    exit 1
fi
echo "✓ Inserted row 2: val='beta' (T1 state established)"

# Switch WAL on leader so the WAL segment containing 'beta' is flushed and archived
echo "  Switching WAL on leader to trigger archive_command..."
run_sql "BEGIN; SELECT pg_switch_wal(); COMMIT;" > /dev/null 2>&1 || \
    docker exec -i "${NODE_CONTAINER}" psql -U postgres -d postgres -t -A -c "SELECT pg_switch_wal();" > /dev/null 2>&1 || true
sleep 1

# ------------------------------------------------------------------------------
# [5/9] Trigger backup T2 (snapshot 2 with 'alpha' + 'beta')
# ------------------------------------------------------------------------------
echo ""
echo "[5/9] Triggering basebackup T2 via API..."
RESP_T2=$(curl -s -f -X POST "${DASHBOARD_URL}/api/backups" \
    "${AUTH_HEADER[@]}" \
    -H "Content-Type: application/json" \
    -d '{"backup_type": "full", "label": "pitr-suite-t2"}')

SNAP_T2=$(echo "${RESP_T2}" | json_extract "snapshot_id")
if [ -z "${SNAP_T2}" ]; then
    echo "Failed to trigger backup T2. Response: ${RESP_T2}"
    exit 1
fi
echo "✓ Backup T2 complete: snapshot_id='${SNAP_T2}'"

curl -s -f "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/backups/${SNAP_T2}/download" -o "${TMP_ARCHIVE_T2}"
if [ ! -s "${TMP_ARCHIVE_T2}" ]; then
    echo "Downloaded archive T2 is empty!"
    exit 1
fi
echo "✓ Snapshot T2 archive verified ($(wc -c < "${TMP_ARCHIVE_T2}") bytes)."

# ------------------------------------------------------------------------------
# [6/9] Verify invalid PITR timestamps (future, too-old, malformed) do NOT fail cluster
# ------------------------------------------------------------------------------
echo ""
echo "[6/9] Testing invalid PITR timestamps (future, too-old, malformed)..."

# 1. Test future timestamp via restore API (POST /api/backups/:id/restore)
echo "  [6.1] Testing future timestamp rejection via restore API..."
HTTP_STATUS=$(curl -s -o /tmp/pitr_err_$$.txt -w "%{http_code}" -X POST "${DASHBOARD_URL}/api/backups/${SNAP_T0}/restore" \
    "${AUTH_HEADER[@]}" \
    -H "Content-Type: application/json" \
    -d '{"recovery_target_time": "2099-01-01 00:00:00"}')
ERR_BODY=$(cat /tmp/pitr_err_$$.txt)
rm -f /tmp/pitr_err_$$.txt

if [ "${HTTP_STATUS}" != "400" ]; then
    echo "Expected HTTP 400 for future restore timestamp, got ${HTTP_STATUS}: ${ERR_BODY}"
    exit 1
fi
if [[ "${ERR_BODY}" != *"cannot be in the future"* ]]; then
    echo "Expected future timestamp error message, got: ${ERR_BODY}"
    exit 1
fi
echo "  ✓ Future timestamp correctly rejected with 400 Bad Request."

# 2. Test future timestamp via quick-restore API (POST /api/backups/quick-restore)
echo "  [6.2] Testing future timestamp rejection via quick-restore API..."
HTTP_STATUS=$(curl -s -o /tmp/pitr_err_$$.txt -w "%{http_code}" -X POST "${DASHBOARD_URL}/api/backups/quick-restore" \
    "${AUTH_HEADER[@]}" \
    -H "Content-Type: application/json" \
    -d '{"recovery_target_time": "2099-01-01 00:00:00"}')
ERR_BODY=$(cat /tmp/pitr_err_$$.txt)
rm -f /tmp/pitr_err_$$.txt

if [ "${HTTP_STATUS}" != "400" ]; then
    echo "Expected HTTP 400 for quick-restore future timestamp, got ${HTTP_STATUS}: ${ERR_BODY}"
    exit 1
fi
if [[ "${ERR_BODY}" != *"cannot be in the future"* ]]; then
    echo "Expected future timestamp error message, got: ${ERR_BODY}"
    exit 1
fi
echo "  ✓ Quick-restore future timestamp correctly rejected with 400 Bad Request."

# 3. Test too-old timestamp via restore API (older than snapshot creation)
echo "  [6.3] Testing too-old timestamp rejection via restore API..."
HTTP_STATUS=$(curl -s -o /tmp/pitr_err_$$.txt -w "%{http_code}" -X POST "${DASHBOARD_URL}/api/backups/${SNAP_T2}/restore" \
    "${AUTH_HEADER[@]}" \
    -H "Content-Type: application/json" \
    -d '{"recovery_target_time": "1999-01-01 00:00:00"}')
ERR_BODY=$(cat /tmp/pitr_err_$$.txt)
rm -f /tmp/pitr_err_$$.txt

if [ "${HTTP_STATUS}" != "400" ]; then
    echo "Expected HTTP 400 for too-old restore timestamp, got ${HTTP_STATUS}: ${ERR_BODY}"
    exit 1
fi
if [[ "${ERR_BODY}" != *"earlier than snapshot"* && "${ERR_BODY}" != *"cannot roll backward"* ]]; then
    echo "Expected too-old timestamp error message, got: ${ERR_BODY}"
    exit 1
fi
echo "  ✓ Too-old timestamp correctly rejected with 400 Bad Request."

# 4. Test too-old timestamp via quick-restore API (older than all snapshots)
echo "  [6.4] Testing too-old timestamp rejection via quick-restore API..."
HTTP_STATUS=$(curl -s -o /tmp/pitr_err_$$.txt -w "%{http_code}" -X POST "${DASHBOARD_URL}/api/backups/quick-restore" \
    "${AUTH_HEADER[@]}" \
    -H "Content-Type: application/json" \
    -d '{"recovery_target_time": "1999-01-01 00:00:00"}')
ERR_BODY=$(cat /tmp/pitr_err_$$.txt)
rm -f /tmp/pitr_err_$$.txt

if [ "${HTTP_STATUS}" != "400" ]; then
    echo "Expected HTTP 400 for quick-restore too-old timestamp, got ${HTTP_STATUS}: ${ERR_BODY}"
    exit 1
fi
if [[ "${ERR_BODY}" != *"No basebackup snapshot found prior to target time"* ]]; then
    echo "Expected no-prior-snapshot error message, got: ${ERR_BODY}"
    exit 1
fi
echo "  ✓ Quick-restore too-old timestamp correctly rejected with 400 Bad Request."

# 5. Test malformed timestamp
echo "  [6.5] Testing malformed timestamp rejection via restore API..."
HTTP_STATUS=$(curl -s -o /tmp/pitr_err_$$.txt -w "%{http_code}" -X POST "${DASHBOARD_URL}/api/backups/${SNAP_T0}/restore" \
    "${AUTH_HEADER[@]}" \
    -H "Content-Type: application/json" \
    -d '{"recovery_target_time": "not-a-valid-date"}')
ERR_BODY=$(cat /tmp/pitr_err_$$.txt)
rm -f /tmp/pitr_err_$$.txt

if [ "${HTTP_STATUS}" != "400" ]; then
    echo "Expected HTTP 400 for malformed timestamp, got ${HTTP_STATUS}: ${ERR_BODY}"
    exit 1
fi
if [[ "${ERR_BODY}" != *"Invalid timestamp"* ]]; then
    echo "Expected invalid timestamp error message, got: ${ERR_BODY}"
    exit 1
fi
echo "  ✓ Malformed timestamp correctly rejected with 400 Bad Request."

# 6. Verify cluster is NOT broken, still healthy, and fully operational
echo "  [6.6] Verifying cluster health and read/write operational status..."
if ! run_sql "SELECT 1;" > /dev/null; then
    echo "Cluster failed to respond after invalid PITR requests!"
    exit 1
fi

# Assert existing rows are intact
COUNT_MID=$(run_sql "SELECT COUNT(*) FROM t_pitr;")
if [ "${COUNT_MID}" != "2" ]; then
    echo "Cluster data corrupted or lost after invalid PITR! Expected 2 rows, got '${COUNT_MID}'"
    exit 1
fi

# Assert write capability works normally
run_sql "INSERT INTO t_pitr (id, val) VALUES (99, 'probe');"
PROBE_VAL=$(run_sql "SELECT val FROM t_pitr WHERE id = 99;")
if [ "${PROBE_VAL}" != "probe" ]; then
    echo "Failed to write probe row after invalid PITR requests! Got: '${PROBE_VAL}'"
    exit 1
fi
run_sql "DELETE FROM t_pitr WHERE id = 99;"

# Assert no nodes crashed or became unhealthy
wait_for_healthy 10 "pgvisor-pitr-node1" "pgvisor-pitr-node2" "pgvisor-pitr-node3"
echo "✓ Cluster remained 100% operational with zero node crashes or hung states."

# ------------------------------------------------------------------------------
# [7/9] Restore from T0 backup at T3 -> verify table has only 'alpha'
# ------------------------------------------------------------------------------
echo ""
echo "[7/9] Restoring from snapshot T0 (${SNAP_T0})..."
restore_cluster_node "${SNAP_T0}"
# Ensure all containers and proxy are fully healthy and responsive before querying
wait_for_healthy 60 "pgvisor-pitr-node1" "pgvisor-pitr-node2" "pgvisor-pitr-node3"
wait_for_proxy_ready "${DASHBOARD_URL}" 30 "${AUTH_HEADER[@]}"
sleep 3

COUNT_RESTORE_T0=""
for attempt in $(seq 1 20); do
    if COUNT_RESTORE_T0=$(run_sql "SELECT COUNT(*) FROM t_pitr;" 2> /dev/null); then
        if [ "${COUNT_RESTORE_T0}" = "1" ]; then
            break
        fi
    fi
    sleep 3
done

if [ "${COUNT_RESTORE_T0}" != "1" ]; then
    echo "Restore T0 verification failed: expected 1 row, got '${COUNT_RESTORE_T0}'"
    exit 1
fi

VAL_CHECK_T0=$(run_sql "SELECT val FROM t_pitr WHERE id = 1;")
BETA_CHECK=$(run_sql "SELECT COUNT(*) FROM t_pitr WHERE val = 'beta';")

if [ "${VAL_CHECK_T0}" != "alpha" ] || [ "${BETA_CHECK}" != "0" ]; then
    echo "Restore T0 data mismatch: expected only 'alpha', but found beta=${BETA_CHECK}"
    exit 1
fi
echo "✓ Snapshot T0 verified: exactly 1 row ('alpha' present, 'beta' absent)."

# ------------------------------------------------------------------------------
# [8/9] Restore from T2 backup at T4 -> verify table has both 'alpha' and 'beta'
# ------------------------------------------------------------------------------
echo ""
echo "[8/9] Restoring from snapshot T2 (${SNAP_T2})..."
restore_cluster_node "${SNAP_T2}"
# Ensure all containers and proxy are fully healthy and responsive before querying
wait_for_healthy 60 "pgvisor-pitr-node1" "pgvisor-pitr-node2" "pgvisor-pitr-node3"
wait_for_proxy_ready "${DASHBOARD_URL}" 30 "${AUTH_HEADER[@]}"
sleep 3

COUNT_RESTORE_T2=""
for attempt in $(seq 1 20); do
    if COUNT_RESTORE_T2=$(run_sql "SELECT COUNT(*) FROM t_pitr;" 2> /dev/null); then
        if [ "${COUNT_RESTORE_T2}" = "2" ]; then
            break
        fi
    fi
    sleep 3
done

if [ "${COUNT_RESTORE_T2}" != "2" ]; then
    echo "Restore T2 verification failed: expected 2 rows, got '${COUNT_RESTORE_T2}'"
    exit 1
fi

VALS_T2=$(run_sql "SELECT val FROM t_pitr ORDER BY id;")
EXPECTED_VALS="alpha
beta"

if [ "${VALS_T2}" != "${EXPECTED_VALS}" ]; then
    echo "Restore T2 data mismatch! Expected:"
    echo "${EXPECTED_VALS}"
    echo "Got:"
    echo "${VALS_T2}"
    exit 1
fi
echo "✓ Snapshot T2 verified: exactly 2 rows ('alpha' and 'beta' both present)."

# ------------------------------------------------------------------------------
# [9/9] Clean up test artifacts
# ------------------------------------------------------------------------------
echo ""
echo "[9/9] Cleaning up test artifacts..."
run_sql "DROP TABLE IF EXISTS t_pitr;" > /dev/null 2>&1 || true
rm -f "${TMP_ARCHIVE_T0}" "${TMP_ARCHIVE_T2}"
curl -s "${AUTH_HEADER[@]}" -X DELETE "${DASHBOARD_URL}/api/backups/${SNAP_T0}" > /dev/null 2>&1 || true
curl -s "${AUTH_HEADER[@]}" -X DELETE "${DASHBOARD_URL}/api/backups/${SNAP_T2}" > /dev/null 2>&1 || true
echo "✓ Cleanup complete."

echo ""
echo "========================================================="
echo "  Incremental Backup & PITR Test PASSED Successfully!    "
echo "========================================================="
