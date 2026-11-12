#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Restore Failure Rollback & Leadership Retention Test
#
# Verifies that when a restore fails (e.g. invalid PITR target time),
# the shadow recovery process fails safely, staging data is cleaned up,
# the production PostgreSQL instance on node1 remains running without downtime,
# node1 retains the leader role, standbys do not trigger failover election,
# and original data remains intact and writable through the proxy.
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
# shellcheck source=tests/lib/cluster.sh
source "${SCRIPT_DIR}/lib/cluster.sh"
# shellcheck source=tests/lib/helper.sh
source "${SCRIPT_DIR}/lib/helper.sh"

# Pre-defined test port & project constants
readonly TEST_PROXY_PORT=7832
readonly TEST_DASHBOARD_PORT=10480
readonly TEST_MINIO_PORT=11400
readonly TEST_MINIO_CONSOLE=11401
readonly PROJECT_NAME="pgvisor-restore-failure"
readonly COMPOSE_FILE="${REPO_ROOT}/composes/docker-compose.restore-failure.yml"

PROXY_HOST="${PGVISOR_HOST:-localhost}"
PROXY_PORT="${PGVISOR_PORT:-${TEST_PROXY_PORT}}"
DASHBOARD_URL="${PGVISOR_DASHBOARD_URL:-http://localhost:${TEST_DASHBOARD_PORT}}"
ADMIN_TOKEN="${PGVISOR_ADMIN_TOKEN:-postgres}"
AUTH_HEADER=()
if [ -n "${ADMIN_TOKEN}" ]; then
    AUTH_HEADER=(-H "Authorization: Bearer ${ADMIN_TOKEN}")
fi

echo "========================================================="
echo "  PgVisor Restore Failure Rollback Verification Test     "
echo "  Project: ${PROJECT_NAME} | Port: ${PROXY_PORT}         "
echo "========================================================="

cleanup() {
    echo "Tearing down cluster ${PROJECT_NAME}..."
    cluster_down "${PROJECT_NAME}" "${COMPOSE_FILE}"
}
trap cleanup EXIT

echo "[0/6] Starting isolated test cluster ${PROJECT_NAME}..."
cluster_down "${PROJECT_NAME}" "${COMPOSE_FILE}"
cluster_up   "${PROJECT_NAME}" "${COMPOSE_FILE}"

echo "Waiting for cluster containers to report healthy..."
wait_for_healthy 120 "${PROJECT_NAME}-minio" "pgvisor-restore-failure-node1" "pgvisor-restore-failure-node2" "pgvisor-restore-failure-node3"
wait_for_proxy_ready "${DASHBOARD_URL}" 60 "${AUTH_HEADER[@]}"

# ------------------------------------------------------------------------------
# [1/6] Ensure cluster is healthy and seed initial data
# ------------------------------------------------------------------------------
echo ""
echo "[1/6] Verifying cluster connectivity and seeding test table..."
if ! run_sql "SELECT 1;" &> /dev/null; then
    echo "Error: Cannot connect to PostgreSQL at ${PROXY_HOST}:${PROXY_PORT}"
    exit 1
fi
echo "✓ PostgreSQL cluster proxy is reachable."

run_sql "DROP TABLE IF EXISTS t_failover_test; CREATE TABLE t_failover_test (id int PRIMARY KEY, val text NOT NULL); INSERT INTO t_failover_test (id, val) VALUES (1, 'original_payload');"
INITIAL_VAL=$(run_sql "SELECT val FROM t_failover_test WHERE id = 1;")
if [ "${INITIAL_VAL}" != "original_payload" ]; then
    echo "Failed to insert initial record 'original_payload'. Got: '${INITIAL_VAL}'"
    exit 1
fi
echo "✓ Table 't_failover_test' created and seeded with: id=1, val='original_payload'"

# ------------------------------------------------------------------------------
# [2/6] Trigger physical basebackup to establish baseline snapshot
# ------------------------------------------------------------------------------
echo ""
echo "[2/6] Creating baseline physical basebackup snapshot..."
BACKUP_RESP=$(curl -s -f -X POST "${DASHBOARD_URL}/api/backups" \
    "${AUTH_HEADER[@]}" \
    -H "Content-Type: application/json" \
    -d '{"backup_type": "full", "label": "test-restore-fail-suite"}')

SNAPSHOT_ID=$(echo "${BACKUP_RESP}" | json_extract "snapshot_id")
if [ -z "${SNAPSHOT_ID}" ]; then
    echo "Failed to create baseline backup. API response: ${BACKUP_RESP}"
    exit 1
fi
echo "✓ Baseline snapshot created: ${SNAPSHOT_ID}"

# ------------------------------------------------------------------------------
# [3/6] Attempt restore with target timestamp that has no covering WAL in archive
# ------------------------------------------------------------------------------
echo ""
TARGET_TIME=$(date -u -d '+2 seconds' '+%Y-%m-%d %H:%M:%S')
echo "[3/6] Attempting restore with uncovered target time (${TARGET_TIME})..."
TMP_RESTORE_OUT="/tmp/restore-fail-$$.json"
HTTP_CODE=$(curl -s -w "%{http_code}" -o "${TMP_RESTORE_OUT}" -X POST "${DASHBOARD_URL}/api/backups/${SNAPSHOT_ID}/restore" \
    "${AUTH_HEADER[@]}" \
    -H "Content-Type: application/json" \
    -d "{\"recovery_target_time\": \"${TARGET_TIME}\"}" || true)

RESTORE_RESP=$(cat "${TMP_RESTORE_OUT}" 2>/dev/null || echo "")
rm -f "${TMP_RESTORE_OUT}"
echo "  HTTP status code: ${HTTP_CODE}"
echo "  Response body: ${RESTORE_RESP}"

# ------------------------------------------------------------------------------
# [4/6] Assert restore failed with HTTP 500 error
# ------------------------------------------------------------------------------
echo ""
echo "[4/6] Verifying restore was rejected with server error..."
if [ "${HTTP_CODE}" != "500" ]; then
    echo "FAILED: Expected HTTP 500 for invalid recovery target time, got: ${HTTP_CODE}"
    exit 1
fi
echo "✓ Restore failed as expected with HTTP 500."

# ------------------------------------------------------------------------------
# [5/6] Assert leader node 1 remains online, healthy, and keeps leadership role
# ------------------------------------------------------------------------------
echo ""
echo "[5/6] Verifying node 1 status and cluster leadership retention..."

# Give any errant election loop a window to fail if it was going to
sleep 3

NODE1_STATUS_RESP=$(docker exec "pgvisor-restore-failure-node1" curl -s http://127.0.0.1:8080/control/status)
NODE1_STATUS=$(echo "${NODE1_STATUS_RESP}" | json_extract "status")
NODE1_ROLE=$(echo "${NODE1_STATUS_RESP}" | json_extract "role")

echo "  Node 1 status: ${NODE1_STATUS}, role: ${NODE1_ROLE}"

if [ "${NODE1_STATUS}" != "running" ]; then
    echo "FAILED: Node 1 status should be 'running' after failed restore, got: '${NODE1_STATUS}'"
    exit 1
fi

if [ "${NODE1_ROLE}" != "leader" ]; then
    echo "FAILED: Node 1 role should still be 'leader' after failed restore, got: '${NODE1_ROLE}'"
    exit 1
fi
echo "✓ Node 1 remained 'running' and maintained 'leader' role without stopping."

# Assert Node 2 did NOT take over leadership
NODE2_STATUS_RESP=$(docker exec "pgvisor-restore-failure-node2" curl -s http://127.0.0.1:8080/control/status)
NODE2_ROLE=$(echo "${NODE2_STATUS_RESP}" | json_extract "role")
echo "  Node 2 role: ${NODE2_ROLE}"
if [ "${NODE2_ROLE}" != "standby" ]; then
    echo "FAILED: Node 2 seized leadership! Expected 'standby', got: '${NODE2_ROLE}'"
    exit 1
fi
echo "✓ Node 2 remained in 'standby' role (no split-brain or false failover)."

# Assert staging directory was completely cleaned up on node 1
STAGING_EXISTS=$(docker exec "pgvisor-restore-failure-node1" sh -c '[ -d /var/lib/postgresql/data/pgdata_staging ] && echo "yes" || echo "no"')
if [ "${STAGING_EXISTS}" = "yes" ]; then
    echo "FAILED: pgdata_staging directory was not cleaned up on node 1!"
    exit 1
fi
echo "✓ Verified pgdata_staging directory was completely removed."

# ------------------------------------------------------------------------------
# [6/6] Assert original data is intact and write traffic continues through proxy
# ------------------------------------------------------------------------------
echo ""
echo "[6/6] Verifying data integrity and proxy write availability..."
VERIFIED_VAL=""
for attempt in 1 2 3 4 5; do
    if VERIFIED_VAL=$(run_sql "SELECT val FROM t_failover_test WHERE id = 1;" 2>/dev/null); then
        if [ "${VERIFIED_VAL}" = "original_payload" ]; then
            break
        fi
    fi
    sleep 1
done

if [ "${VERIFIED_VAL}" != "original_payload" ]; then
    echo "FAILED: Original data in 't_failover_test' was corrupted or lost! Expected 'original_payload', got: '${VERIFIED_VAL}'"
    exit 1
fi
echo "✓ Original data preserved: id=1, val='${VERIFIED_VAL}'"

# Execute a write statement through the proxy to ensure read/write traffic is fully operational
run_sql "INSERT INTO t_failover_test (id, val) VALUES (2, 'write_after_failed_restore');"
TOTAL_ROWS=$(run_sql "SELECT count(*) FROM t_failover_test;")
if [ "${TOTAL_ROWS}" != "2" ]; then
    echo "FAILED: Could not write new row through proxy after failed restore. Row count: ${TOTAL_ROWS}"
    exit 1
fi
echo "✓ Read/write traffic verified: successfully inserted row 2 through proxy."

echo ""
echo "Cleaning up test table..."
run_sql "DROP TABLE IF EXISTS t_failover_test;" > /dev/null 2>&1 || true
curl -s "${AUTH_HEADER[@]}" -X DELETE "${DASHBOARD_URL}/api/backups/${SNAPSHOT_ID}" > /dev/null 2>&1 || true
echo "✓ Cleanup complete."

echo ""
echo "========================================================="
echo "  Restore Failure Rollback Test PASSED Successfully!     "
echo "========================================================="
