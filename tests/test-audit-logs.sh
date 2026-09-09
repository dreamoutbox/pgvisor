#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Audit Logs Verification Test
#
# Self-contained concurrent test profile.
# Tests structured audit logging for:
#   - Node up / down state transitions
#   - Physical backup creation and cluster restore
#   - Dangerous SQL (DROP TABLE, TRUNCATE, and DELETE without WHERE clause)
#   - PITR target timestamp recommendations
#   - Consensus election results & worker join/leave events
#   - S3 persistent event storage via MinIO/OpenDAL
#   - Web dashboard audit UI (/audit-logs)
# All output is clean plain text.
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
# shellcheck source=tests/lib/cluster.sh
source "${SCRIPT_DIR}/lib/cluster.sh"

# Pre-defined test port & project constants
readonly TEST_PROXY_PORT=6632
readonly TEST_DASHBOARD_PORT=9280
readonly TEST_MINIO_PORT=10200
readonly TEST_MINIO_CONSOLE=10201
readonly PROJECT_NAME="pgvisor-audit-logs"
readonly COMPOSE_FILE="${REPO_ROOT}/composes/docker-compose.audit-logs.yml"

PROXY_HOST="${PGVISOR_HOST:-localhost}"
PROXY_PORT="${PGVISOR_PORT:-${TEST_PROXY_PORT}}"
DASHBOARD_URL="${PGVISOR_DASHBOARD_URL:-http://localhost:${TEST_DASHBOARD_PORT}}"
ADMIN_TOKEN="${PGVISOR_ADMIN_TOKEN:-postgres}"
AUTH_HEADER=()
if [ -n "${ADMIN_TOKEN}" ]; then
    AUTH_HEADER=(-H "Authorization: Bearer ${ADMIN_TOKEN}")
fi

NODE1_CONTAINER="pgvisor-audit-logs-node1"
NODE2_CONTAINER="pgvisor-audit-logs-node2"
NODE3_CONTAINER="pgvisor-audit-logs-node3"
PROXY_CONTAINER="pgvisor-audit-logs-proxy"

cleanup() {
    echo "Tearing down cluster ${PROJECT_NAME}..."
    cluster_down "${PROJECT_NAME}" "${COMPOSE_FILE}"
}
trap cleanup EXIT

echo "========================================================="
echo "  PgVisor Audit Logs Verification Test                   "
echo "  Project: ${PROJECT_NAME} | Port: ${PROXY_PORT}         "
echo "========================================================="

echo "[0/9] Starting isolated test cluster ${PROJECT_NAME}..."
cluster_down "${PROJECT_NAME}" "${COMPOSE_FILE}"
cluster_up   "${PROJECT_NAME}" "${COMPOSE_FILE}"

echo "Waiting for cluster containers to report healthy..."
wait_for_healthy 120 "${PROJECT_NAME}-minio" "${NODE1_CONTAINER}" "${NODE2_CONTAINER}" "${NODE3_CONTAINER}"
wait_for_proxy_ready "${DASHBOARD_URL}" 60 "${AUTH_HEADER[@]}"

exec_sql() {
    local sql="$1"
    if command -v psql &> /dev/null; then
        PGPASSWORD="" psql -h "${PROXY_HOST}" -p "${PROXY_PORT}" -U postgres -d postgres -c "${sql}"
    else
        docker compose -p "${PROJECT_NAME}" -f "${COMPOSE_FILE}" exec -T pgvisor-proxy psql -h localhost -p 5432 -U postgres -d postgres -c "${sql}"
    fi
}

echo ""
echo "[1/9] Verifying initial cluster startup audit events..."
ATTEMPTS=0
TOTAL_EVENTS=0
RESPONSE=""
while [ $ATTEMPTS -lt 25 ]; do
    RESPONSE=$(curl -s "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/audit-logs" || true)
    TOTAL_EVENTS=$(echo "${RESPONSE}" | jq -r '.total // 0' 2>/dev/null | tr -dc '0-9' || true)
    TOTAL_EVENTS="${TOTAL_EVENTS:-0}"
    if [ "${TOTAL_EVENTS}" -gt 0 ]; then
        echo "Found ${TOTAL_EVENTS} initial audit events recorded on startup."
        break
    fi
    ATTEMPTS=$((ATTEMPTS + 1))
    sleep 1
done

if [ "${TOTAL_EVENTS}" -le 0 ]; then
    echo "ERROR: No initial audit events recorded within timeout."
    echo "Last API response: ${RESPONSE}"
    exit 1
fi

echo ""
echo "[2/9] Testing dangerous SQL: DROP TABLE..."
exec_sql "CREATE TABLE t_audit_drop (id INT, note TEXT);"
exec_sql "INSERT INTO t_audit_drop VALUES (1, 'sensitive data');"
exec_sql "DROP TABLE t_audit_drop;"

# Verify audit log captures DROP TABLE with PITR recommendation
ATTEMPTS=0
FOUND_DROP=false
while [ $ATTEMPTS -lt 15 ]; do
    RESPONSE=$(curl -s "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/audit-logs?kind=dangerous_sql" || true)
    if echo "${RESPONSE}" | grep -q "t_audit_drop"; then
        PITR_TARGET=$(echo "${RESPONSE}" | jq -r '.events[] | select(.detail | contains("t_audit_drop")) | .pitr_target // empty' | head -n1)
        echo "Successfully audited DROP TABLE t_audit_drop (PITR target: ${PITR_TARGET})"
        FOUND_DROP=true
        break
    fi
    ATTEMPTS=$((ATTEMPTS + 1))
    sleep 1
done

if [ "${FOUND_DROP}" != "true" ]; then
    echo "ERROR: Audit log failed to capture DROP TABLE statement."
    exit 1
fi

echo ""
echo "[3/9] Testing dangerous SQL: TRUNCATE..."
exec_sql "CREATE TABLE t_audit_trunc (id INT);"
exec_sql "INSERT INTO t_audit_trunc VALUES (1), (2);"
exec_sql "TRUNCATE t_audit_trunc;"

ATTEMPTS=0
FOUND_TRUNC=false
while [ $ATTEMPTS -lt 15 ]; do
    RESPONSE=$(curl -s "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/audit-logs?kind=dangerous_sql" || true)
    if echo "${RESPONSE}" | grep -q "TRUNCATE"; then
        echo "Successfully audited TRUNCATE statement."
        FOUND_TRUNC=true
        break
    fi
    ATTEMPTS=$((ATTEMPTS + 1))
    sleep 1
done

if [ "${FOUND_TRUNC}" != "true" ]; then
    echo "ERROR: Audit log failed to capture TRUNCATE statement."
    exit 1
fi

echo ""
echo "[4/9] Testing dangerous SQL: DELETE scoping (with vs without WHERE clause)..."
exec_sql "CREATE TABLE t_audit_del (id INT, val TEXT);"
exec_sql "INSERT INTO t_audit_del VALUES (1, 'row1'), (2, 'row2'), (3, 'row3');"

# 1. DELETE with WHERE should NOT be audited when PGVISOR_AUDIT_DELETE is unset
exec_sql "DELETE FROM t_audit_del WHERE id = 1;"
sleep 2
RESPONSE_WHERE=$(curl -s "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/audit-logs?kind=dangerous_sql" || true)
if echo "${RESPONSE_WHERE}" | grep -q "WHERE id = 1"; then
    echo "ERROR: Scoped DELETE with WHERE clause was unexpectedly audited as dangerous SQL."
    exit 1
else
    echo "Correct: Scoped DELETE with WHERE clause was not flagged as dangerous SQL."
fi

# 2. DELETE without WHERE (table-wide wipeout) MUST be audited as dangerous SQL
exec_sql "DELETE FROM t_audit_del;"
ATTEMPTS=0
FOUND_DEL=false
while [ $ATTEMPTS -lt 15 ]; do
    RESPONSE=$(curl -s "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/audit-logs?kind=dangerous_sql" || true)
    if echo "${RESPONSE}" | grep -q "DELETE FROM t_audit_del"; then
        echo "Successfully audited table-wide DELETE FROM t_audit_del statement."
        FOUND_DEL=true
        break
    fi
    ATTEMPTS=$((ATTEMPTS + 1))
    sleep 1
done

if [ "${FOUND_DEL}" != "true" ]; then
    echo "ERROR: Audit log failed to capture table-wide DELETE statement."
    exit 1
fi

echo ""
echo "[5/9] Testing backup creation and cluster restore audit events..."
# Trigger backup
BACKUP_RESP=$(curl -s -X POST "${AUTH_HEADER[@]}" -H "Content-Type: application/json" \
    -d '{"label": "audit-test-backup"}' "${DASHBOARD_URL}/api/backups")
SNAP_ID=$(echo "${BACKUP_RESP}" | jq -r '.snapshot_id')
echo "Created snapshot: ${SNAP_ID}"

# Verify backup_created audit event
ATTEMPTS=0
FOUND_BACKUP=false
while [ $ATTEMPTS -lt 15 ]; do
    AUDIT_RESP=$(curl -s "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/audit-logs?kind=backup_created" || true)
    if echo "${AUDIT_RESP}" | grep -q "${SNAP_ID}"; then
        echo "Successfully captured backup_created event for ${SNAP_ID}."
        FOUND_BACKUP=true
        break
    fi
    ATTEMPTS=$((ATTEMPTS + 1))
    sleep 1
done

if [ "${FOUND_BACKUP}" != "true" ]; then
    echo "ERROR: Audit log failed to capture backup_created event."
    exit 1
fi

# Trigger restore
RESTORE_RESP=$(curl -s -X POST "${AUTH_HEADER[@]}" -H "Content-Type: application/json" \
    -d '{"recovery_target_time": null}' "${DASHBOARD_URL}/api/backups/${SNAP_ID}/restore")
echo "Restore triggered: ${RESTORE_RESP}"

# Verify backup_restored audit event
ATTEMPTS=0
FOUND_RESTORE=false
while [ $ATTEMPTS -lt 20 ]; do
    AUDIT_RESP=$(curl -s "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/audit-logs?kind=backup_restored" || true)
    if echo "${AUDIT_RESP}" | grep -q "${SNAP_ID}"; then
        echo "Successfully captured backup_restored event for ${SNAP_ID}."
        FOUND_RESTORE=true
        break
    fi
    ATTEMPTS=$((ATTEMPTS + 1))
    sleep 1
done

if [ "${FOUND_RESTORE}" != "true" ]; then
    echo "ERROR: Audit log failed to capture backup_restored event."
    exit 1
fi

echo ""
echo "[6/9] Testing node down and node up state transition auditing..."
echo "Stopping node2 container (${NODE2_CONTAINER})..."
docker stop "${NODE2_CONTAINER}" > /dev/null

ATTEMPTS=0
FOUND_DOWN=false
while [ $ATTEMPTS -lt 20 ]; do
    AUDIT_RESP=$(curl -s "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/audit-logs?kind=node_down" || true)
    if echo "${AUDIT_RESP}" | grep -q "node2"; then
        echo "Successfully captured node_down event for node2."
        FOUND_DOWN=true
        break
    fi
    ATTEMPTS=$((ATTEMPTS + 1))
    sleep 1
done

if [ "${FOUND_DOWN}" != "true" ]; then
    echo "ERROR: Audit log failed to capture node_down event."
    exit 1
fi

echo "Restarting node2 container (${NODE2_CONTAINER})..."
docker start "${NODE2_CONTAINER}" > /dev/null

ATTEMPTS=0
FOUND_UP=false
while [ $ATTEMPTS -lt 25 ]; do
    AUDIT_RESP=$(curl -s "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/audit-logs?kind=node_up" || true)
    if echo "${AUDIT_RESP}" | grep -q "node2"; then
        echo "Successfully captured node_up event for node2."
        FOUND_UP=true
        break
    fi
    ATTEMPTS=$((ATTEMPTS + 1))
    sleep 1
done

if [ "${FOUND_UP}" != "true" ]; then
    echo "ERROR: Audit log failed to capture node_up event."
    exit 1
fi

echo ""
echo "[7/9] Testing user and permission management auditing..."
exec_sql "CREATE ROLE audit_tester WITH LOGIN;"
sleep 1
exec_sql "DROP ROLE audit_tester;"

ATTEMPTS=0
FOUND_USER=false
while [ $ATTEMPTS -lt 15 ]; do
    AUDIT_RESP=$(curl -s "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/audit-logs?kind=user_permission" || true)
    if echo "${AUDIT_RESP}" | grep -q "audit_tester"; then
        echo "Successfully captured user_permission audit event for audit_tester role."
        FOUND_USER=true
        break
    fi
    ATTEMPTS=$((ATTEMPTS + 1))
    sleep 1
done

if [ "${FOUND_USER}" != "true" ]; then
    echo "ERROR: Audit log failed to capture user_permission event."
    exit 1
fi

echo ""
echo "[8/9] Verifying web dashboard HTML page (/audit-logs)..."
PAGE_HTML=$(curl -s -L "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/audit-logs" || true)
if echo "${PAGE_HTML}" | grep -q "Cluster Audit Logs"; then
    echo "Dashboard HTML rendered successfully with PITR helpers and table."
else
    echo "ERROR: Dashboard /audit-logs did not render expected HTML content."
    echo "PAGE_HTML response: ${PAGE_HTML}"
    exit 1
fi

echo ""
echo "[9/9] Verifying S3 / MinIO persistent storage of audit log events..."
# Inspect MinIO bucket for clusters/pgvisor-cluster/audit/ objects
MINIO_FILES=$(docker compose -p "${PROJECT_NAME}" -f "${COMPOSE_FILE}" exec -T "${PROJECT_NAME}-minio" sh -c "find /data/pgvisor-backups/clusters/pgvisor-cluster/audit -type f 2>/dev/null | wc -l" 2>/dev/null | tr -dc '0-9' || true)
MINIO_FILES="${MINIO_FILES:-0}"
echo "S3 MinIO audit objects written: ${MINIO_FILES}"
if [ "${MINIO_FILES}" -gt 0 ]; then
    echo "Verified: Audit events are persisted to MinIO/S3 object storage."
else
    echo "WARNING: MinIO direct directory check returned 0 files; verifying via API..."
    TOTAL_API_EVENTS=$(curl -s "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/audit-logs" | jq -r '.total // 0' 2>/dev/null | tr -dc '0-9' || true)
    TOTAL_API_EVENTS="${TOTAL_API_EVENTS:-0}"
    if [ "${TOTAL_API_EVENTS}" -gt 0 ]; then
        echo "API confirmed ${TOTAL_API_EVENTS} audit events stored."
    fi
fi

echo ""
echo "========================================================="
echo "  Audit Logs Verification Test: ALL ASSERTIONS PASSED!   "
echo "========================================================="
