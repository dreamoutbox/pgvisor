#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Dashboard Node Inspection & Table Row Mutation Test
#
# Self-contained integration test profile.
# Tests:
#   1. Node stdout/stderr logs inspection via dashboard API (/api/nodes/:id/logs)
#   2. Key diagnostic file inspection (postgresql.conf, pg_hba.conf, postmaster.pid, standby.signal)
#   3. Table schema & primary key discovery (/api/tables/:table/schema)
#   4. Secure table row mutation:
#      - Row editing (/api/tables/:table/rows PUT)
#      - Row deletion (/api/tables/:table/rows DELETE)
#   5. Central AuditLog verification for row mutations
# All output is clean plain text.
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
# shellcheck source=tests/lib/cluster.sh
source "${SCRIPT_DIR}/lib/cluster.sh"

readonly TEST_PROXY_PORT=7732
readonly TEST_DASHBOARD_PORT=10380
readonly TEST_MINIO_PORT=11300
readonly TEST_MINIO_CONSOLE=11301
readonly PROJECT_NAME="pgvisor-tables-nodes"
readonly COMPOSE_FILE="${REPO_ROOT}/composes/docker-compose.tables-nodes.yml"

PROXY_HOST="${PGVISOR_HOST:-localhost}"
PROXY_PORT="${PGVISOR_PORT:-${TEST_PROXY_PORT}}"
DASHBOARD_URL="${PGVISOR_DASHBOARD_URL:-http://localhost:${TEST_DASHBOARD_PORT}}"
ADMIN_TOKEN="${PGVISOR_ADMIN_TOKEN:-postgres}"
AUTH_HEADER=()
if [ -n "${ADMIN_TOKEN}" ]; then
    AUTH_HEADER=(-H "Authorization: Bearer ${ADMIN_TOKEN}")
fi

NODE1_CONTAINER="pgvisor-tables-nodes-node1"
NODE2_CONTAINER="pgvisor-tables-nodes-node2"
NODE3_CONTAINER="pgvisor-tables-nodes-node3"
PROXY_CONTAINER="pgvisor-tables-nodes-proxy"

trap cleanup EXIT

echo "========================================================="
echo "  PgVisor Dashboard Inspection & Row Mutation Test       "
echo "  Project: ${PROJECT_NAME} | Port: ${PROXY_PORT}         "
echo "========================================================="

echo "[0/8] Starting isolated test cluster ${PROJECT_NAME}..."
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
echo "[1/8] Discovering cluster nodes and active roles..."
NODES_JSON=""
for attempt in $(seq 1 30); do
    NODES_JSON=$(curl -s "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/nodes" || true)
    NODE_COUNT=$(echo "${NODES_JSON}" | jq -r 'length // 0' 2>/dev/null || echo "0")
    if [ "${NODE_COUNT}" -ge 3 ]; then
        break
    fi
    sleep 1
done

if [ "${NODE_COUNT}" -lt 3 ]; then
    echo "Error: Expected at least 3 cluster nodes, found ${NODE_COUNT}"
    echo "Payload: ${NODES_JSON}"
    exit 1
fi

LEADER_ID=$(echo "${NODES_JSON}" | jq -r '.[] | select(.role == "Leader" or .role == "leader") | .id' | head -n 1)
STANDBY_ID=$(echo "${NODES_JSON}" | jq -r '.[] | select(.role == "Standby" or .role == "standby" or .role == "Follower" or .role == "follower") | .id' | head -n 1)

echo "Discovered cluster nodes: Leader ID=${LEADER_ID}, Standby ID=${STANDBY_ID}"

echo ""
echo "[2/8] Testing node logs inspection via dashboard API..."
LOGS_RESP=$(curl -s "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/nodes/${LEADER_ID}/logs?limit=10")
LOGS_COUNT=$(echo "${LOGS_RESP}" | jq -r '.entries | length // 0' 2>/dev/null || echo "0")
if [ "${LOGS_COUNT}" -eq 0 ]; then
    echo "Warning: Leader logs returned 0 entries initially, re-querying..."
    sleep 2
    LOGS_RESP=$(curl -s "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/nodes/${LEADER_ID}/logs?limit=10")
    LOGS_COUNT=$(echo "${LOGS_RESP}" | jq -r '.entries | length // 0' 2>/dev/null || echo "0")
fi
echo "Leader node log entries retrieved: ${LOGS_COUNT}"
RESP_NODE_ID=$(echo "${LOGS_RESP}" | jq -r '.node_id')
if [ "${RESP_NODE_ID}" != "${LEADER_ID}" ]; then
    echo "Error: Expected node_id=${LEADER_ID}, got ${RESP_NODE_ID}"
    exit 1
fi

echo ""
echo "[3/8] Testing diagnostic and configuration file inspection..."
# 1. postgresql.conf
CONF_RESP=$(curl -s "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/nodes/${LEADER_ID}/config/postgresql_conf")
CONF_NAME=$(echo "${CONF_RESP}" | jq -r '.filename')
CONF_CONTENT=$(echo "${CONF_RESP}" | jq -r '.content')
if [ "${CONF_NAME}" != "postgresql.conf" ] || [[ ! "${CONF_CONTENT}" =~ (listen_addresses|port|shared_buffers) ]]; then
    echo "Error: Failed to inspect postgresql.conf for node ${LEADER_ID}"
    echo "Response: ${CONF_RESP}"
    exit 1
fi
echo "Verified: postgresql.conf successfully inspected"

# 2. pg_hba.conf
HBA_RESP=$(curl -s "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/nodes/${LEADER_ID}/config/pg_hba_conf")
HBA_NAME=$(echo "${HBA_RESP}" | jq -r '.filename')
HBA_CONTENT=$(echo "${HBA_RESP}" | jq -r '.content')
if [ "${HBA_NAME}" != "pg_hba.conf" ] || [[ ! "${HBA_CONTENT}" =~ (local|host) ]]; then
    echo "Error: Failed to inspect pg_hba.conf for node ${LEADER_ID}"
    echo "Response: ${HBA_RESP}"
    exit 1
fi
echo "Verified: pg_hba.conf successfully inspected"

# 3. postmaster.pid
PID_RESP=$(curl -s "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/nodes/${LEADER_ID}/config/postmaster_pid")
PID_NAME=$(echo "${PID_RESP}" | jq -r '.filename')
PID_CONTENT=$(echo "${PID_RESP}" | jq -r '.content')
if [ "${PID_NAME}" != "postmaster.pid" ] || [ -z "${PID_CONTENT}" ]; then
    echo "Error: Failed to inspect postmaster.pid for node ${LEADER_ID}"
    echo "Response: ${PID_RESP}"
    exit 1
fi
echo "Verified: postmaster.pid successfully inspected"

# 4. standby.signal on standby node
STANDBY_SIG_RESP=$(curl -s "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/nodes/${STANDBY_ID}/config/standby_signal")
SIG_NAME=$(echo "${STANDBY_SIG_RESP}" | jq -r '.filename')
if [ "${SIG_NAME}" != "standby.signal" ]; then
    echo "Error: Expected standby.signal on standby node ${STANDBY_ID}"
    echo "Response: ${STANDBY_SIG_RESP}"
    exit 1
fi
echo "Verified: standby.signal successfully inspected on standby node ${STANDBY_ID}"

echo ""
echo "[4/8] Creating test table and seeding initial rows..."
exec_sql "
    DROP TABLE IF EXISTS public.inventory;
    CREATE TABLE public.inventory (
        id SERIAL PRIMARY KEY,
        sku VARCHAR(64) NOT NULL,
        quantity INT NOT NULL,
        description TEXT
    );
    INSERT INTO public.inventory (sku, quantity, description) VALUES
        ('ITEM-001', 10, 'Initial Item 1'),
        ('ITEM-002', 20, 'Initial Item 2'),
        ('ITEM-003', 30, 'Initial Item 3');
"

echo "Waiting for schema and data to propagate to standby..."
sleep 2

echo ""
echo "[5/8] Verifying table schema and primary key discovery..."
SCHEMA_RESP=$(curl -s "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/tables/inventory/schema")
IS_ID_PK=$(echo "${SCHEMA_RESP}" | jq -r '.[] | select(.name == "id") | .is_primary_key')
if [ "${IS_ID_PK}" != "true" ]; then
    echo "Error: 'id' column was not detected as primary key!"
    echo "Schema response: ${SCHEMA_RESP}"
    exit 1
fi
echo "Verified: Primary key detected correctly on 'id'"

DATA_RESP=$(curl -s "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/tables/inventory/data")
DATA_PKS=$(echo "${DATA_RESP}" | jq -r '.primary_keys[]')
if [ "${DATA_PKS}" != "id" ]; then
    echo "Error: Expected primary_keys=['id'], got ${DATA_PKS}"
    exit 1
fi
TOTAL_ROWS=$(echo "${DATA_RESP}" | jq -r '.total_rows')
echo "Verified: Table data returns primary_keys=['id'], total_rows=${TOTAL_ROWS}"

echo ""
echo "[6/8] Executing table row update via dashboard API..."
UPDATE_RESP=$(curl -s -X PUT "${AUTH_HEADER[@]}" \
    -H "Content-Type: application/json" \
    -d '{"primary_keys": {"id": 2}, "values": {"quantity": 99, "description": "Modified Item 2"}}' \
    "${DASHBOARD_URL}/api/tables/inventory/rows")

UPDATE_STATUS=$(echo "${UPDATE_RESP}" | jq -r '.status // "error"')
AFFECTED=$(echo "${UPDATE_RESP}" | jq -r '.affected_rows // 0')
if [ "${UPDATE_STATUS}" != "ok" ] || [ "${AFFECTED}" -ne 1 ]; then
    echo "Error: Row update failed!"
    echo "Response: ${UPDATE_RESP}"
    exit 1
fi
echo "Row update API succeeded: ${UPDATE_RESP}"

echo "Verifying updated row in database..."
UPDATED_VERIFIED=false
for attempt in $(seq 1 10); do
    READ_OUTPUT=$(exec_sql "SELECT quantity, description FROM public.inventory WHERE id = 2;" 2>&1 || true)
    if echo "${READ_OUTPUT}" | grep -q "99" && echo "${READ_OUTPUT}" | grep -q "Modified Item 2"; then
        UPDATED_VERIFIED=true
        break
    fi
    sleep 1
done

if [ "${UPDATED_VERIFIED}" != "true" ]; then
    echo "Error: Updated row values not reflected in database!"
    echo "Output: ${READ_OUTPUT}"
    exit 1
fi
echo "Verified: Row id=2 updated to quantity=99, description='Modified Item 2'"

echo ""
echo "[7/8] Executing table row deletion via dashboard API..."
DELETE_RESP=$(curl -s -X DELETE "${AUTH_HEADER[@]}" \
    -H "Content-Type: application/json" \
    -d '{"primary_keys": {"id": 1}}' \
    "${DASHBOARD_URL}/api/tables/inventory/rows")

DELETE_STATUS=$(echo "${DELETE_RESP}" | jq -r '.status // "error"')
DELETE_AFFECTED=$(echo "${DELETE_RESP}" | jq -r '.affected_rows // 0')
if [ "${DELETE_STATUS}" != "ok" ] || [ "${DELETE_AFFECTED}" -ne 1 ]; then
    echo "Error: Row delete failed!"
    echo "Response: ${DELETE_RESP}"
    exit 1
fi
echo "Row delete API succeeded: ${DELETE_RESP}"

echo "Verifying deleted row is gone from database..."
DELETED_VERIFIED=false
for attempt in $(seq 1 10); do
    COUNT_OUTPUT=$(exec_sql "SELECT count(*) FROM public.inventory WHERE id = 1;" 2>&1 || true)
    if echo "${COUNT_OUTPUT}" | grep -qE "(^|[[:space:]])0([[:space:]]|$)"; then
        DELETED_VERIFIED=true
        break
    fi
    sleep 1
done

if [ "${DELETED_VERIFIED}" != "true" ]; then
    echo "Error: Deleted row still exists in database!"
    echo "Output: ${COUNT_OUTPUT}"
    exit 1
fi
echo "Verified: Row id=1 successfully deleted from database"

echo ""
echo "[8/8] Verifying audit log records for row mutations..."
AUDIT_RESP=$(curl -s "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/audit-logs?limit=20")
HAS_DELETE_AUDIT=$(echo "${AUDIT_RESP}" | jq -r '.events[] | select(.detail | contains("Deleted row from table '\''inventory'\''")) | .detail' | head -n 1)
HAS_UPDATE_AUDIT=$(echo "${AUDIT_RESP}" | jq -r '.events[] | select(.detail | contains("Updated row in table '\''inventory'\''")) | .detail' | head -n 1)

if [ -z "${HAS_DELETE_AUDIT}" ]; then
    echo "Error: Audit log missing delete row entry!"
    echo "Audit logs response: ${AUDIT_RESP}"
    exit 1
fi
echo "Verified audit entry: ${HAS_DELETE_AUDIT}"

if [ -z "${HAS_UPDATE_AUDIT}" ]; then
    echo "Error: Audit log missing update row entry!"
    echo "Audit logs response: ${AUDIT_RESP}"
    exit 1
fi
echo "Verified audit entry: ${HAS_UPDATE_AUDIT}"

echo ""
echo "========================================================="
echo "  All Dashboard Inspection & Row Mutation tests PASSED!  "
echo "========================================================="
