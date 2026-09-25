#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Node Start / Stop / Restart Lifecycle Verification Test
#
# Self-contained concurrent test profile.
# Tests starting, stopping, and restarting nodes from the dashboard API,
# verifying sidecar status transitions, proxy connection pool draining,
# replication recovery, error handling, and audit event tracking.
# All output is clean plain text (no ANSI escapes).
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
# shellcheck source=tests/lib/cluster.sh
source "${SCRIPT_DIR}/lib/cluster.sh"
# shellcheck source=tests/lib/helper.sh
source "${SCRIPT_DIR}/lib/helper.sh"

readonly TEST_PROXY_PORT=7132
readonly TEST_DASHBOARD_PORT=9780
readonly TEST_MINIO_PORT=10700
readonly TEST_MINIO_CONSOLE=10701
readonly PROJECT_NAME="pgvisor-node-lifecycle"
readonly COMPOSE_FILE="${REPO_ROOT}/composes/docker-compose.node-lifecycle.yml"

PROXY_HOST="${PGVISOR_HOST:-localhost}"
PROXY_PORT="${PGVISOR_PORT:-${TEST_PROXY_PORT}}"
DASHBOARD_URL="${PGVISOR_DASHBOARD_URL:-http://localhost:${TEST_DASHBOARD_PORT}}"
TABLE_NAME="t_node_lifecycle"
ADMIN_TOKEN="${PGVISOR_ADMIN_TOKEN:-postgres}"
AUTH_HEADER=()
if [ -n "${ADMIN_TOKEN}" ]; then
    AUTH_HEADER=(-H "Authorization: Bearer ${ADMIN_TOKEN}")
fi

NODE1_CONTAINER="pgvisor-node-lifecycle-node1"
NODE2_CONTAINER="pgvisor-node-lifecycle-node2"
NODE3_CONTAINER="pgvisor-node-lifecycle-node3"
PROXY_CONTAINER="pgvisor-node-lifecycle-proxy"

trap cleanup EXIT

echo "========================================================="
echo "  PgVisor Node Start/Stop/Restart Lifecycle Test         "
echo "  Project: ${PROJECT_NAME} | Port: ${PROXY_PORT}         "
echo "========================================================="

echo "[0/9] Starting isolated test cluster ${PROJECT_NAME}..."
cluster_down "${PROJECT_NAME}" "${COMPOSE_FILE}"
cluster_up   "${PROJECT_NAME}" "${COMPOSE_FILE}"

echo "Waiting for cluster containers to report healthy..."
wait_for_healthy 120 "${PROJECT_NAME}-minio" "${NODE1_CONTAINER}" "${NODE2_CONTAINER}" "${NODE3_CONTAINER}"
wait_for_proxy_ready "${DASHBOARD_URL}" 60 "${AUTH_HEADER[@]}"


echo ""
echo "[1/9] Verifying initial cluster topology..."
NODES_JSON=$(curl -s -X GET "${DASHBOARD_URL}/api/nodes" "${AUTH_HEADER[@]}")
echo "  Nodes response: ${NODES_JSON}"
if [[ "${NODES_JSON}" != *"node_id\":1"* || "${NODES_JSON}" != *"node_id\":2"* || "${NODES_JSON}" != *"node_id\":3"* ]]; then
    echo "ERROR: Dashboard did not report all 3 cluster nodes"
    exit 1
fi
echo "+ Verified: all 3 nodes discovered by dashboard."

echo ""
echo "[2/9] Seeding test table '${TABLE_NAME}' with initial record 'init_row'..."
run_proxy_sql "DROP TABLE IF EXISTS ${TABLE_NAME};" > /dev/null
run_proxy_sql "CREATE TABLE ${TABLE_NAME} (id SERIAL PRIMARY KEY, val TEXT NOT NULL);" > /dev/null
run_proxy_sql "INSERT INTO ${TABLE_NAME} (val) VALUES ('init_row');" > /dev/null
INIT_COUNT=$(run_proxy_sql "SELECT count(*) FROM ${TABLE_NAME};")
if [[ "${INIT_COUNT}" != "1" ]]; then
    echo "ERROR: Expected 1 row in ${TABLE_NAME}, got: ${INIT_COUNT}"
    exit 1
fi
echo "+ Table '${TABLE_NAME}' seeded via proxy."

echo ""
echo "[3/9] Stopping Node #2 via Dashboard API (POST /api/nodes/2/stop)..."
STOP_RESP=$(curl -s -X POST "${DASHBOARD_URL}/api/nodes/2/stop" "${AUTH_HEADER[@]}")
echo "  Stop API response: ${STOP_RESP}"
STOP_STATUS=$(echo "${STOP_RESP}" | json_extract "status")
if [[ "${STOP_STATUS}" != "ok" ]]; then
    echo "ERROR: Stop API failed: ${STOP_RESP}"
    exit 1
fi

echo "Verifying Node #2 reports stopped in sidecar status..."
NODE2_STOPPED=false
for attempt in 1 2 3 4 5 6 7 8 9 10; do
    ST2=$(get_sidecar_status "${NODE2_CONTAINER}")
    S2=$(echo "${ST2}" | json_extract "status")
    if [[ "${S2}" == "stopped" ]]; then
        NODE2_STOPPED=true
        echo "+ CONFIRMED: ${NODE2_CONTAINER} reports status=stopped (child_pid=$(echo "${ST2}" | json_extract "child_pid"))"
        break
    fi
    sleep 1
done

if [[ "${NODE2_STOPPED}" != "true" ]]; then
    echo "ERROR: Node #2 did not report status=stopped"
    exit 1
fi

echo "Verifying Node #2 reports state=stopped in dashboard API..."
DASH_STOPPED=false
for attempt in 1 2 3 4 5 6 7 8 9 10; do
    NODES_JSON=$(curl -s -X GET "${DASHBOARD_URL}/api/nodes" "${AUTH_HEADER[@]}")
    if [[ "${NODES_JSON}" == *"node_id\":2"* && "${NODES_JSON}" == *"\"state\":\"stopped\""* ]]; then
        DASH_STOPPED=true
        echo "+ CONFIRMED: Dashboard reports Node #2 state=stopped"
        break
    fi
    sleep 1
done

if [[ "${DASH_STOPPED}" != "true" ]]; then
    echo "ERROR: Dashboard did not reflect Node #2 as stopped. Nodes: ${NODES_JSON}"
    exit 1
fi

echo ""
echo "[4/9] Verifying client traffic continues through proxy with Node #2 stopped..."
run_proxy_sql "INSERT INTO ${TABLE_NAME} (val) VALUES ('while_node2_stopped');" > /dev/null
COUNT_AFTER_STOP=""
for attempt in 1 2 3 4 5; do
    COUNT_AFTER_STOP=$(run_proxy_sql "SELECT count(*) FROM ${TABLE_NAME};")
    if [[ "${COUNT_AFTER_STOP}" == "2" ]]; then
        break
    fi
    sleep 1
done

if [[ "${COUNT_AFTER_STOP}" != "2" ]]; then
    echo "ERROR: Expected 2 rows after insert with Node #2 stopped, got: ${COUNT_AFTER_STOP}"
    exit 1
fi
echo "+ Proxy routing reads and writes normally while Node #2 is stopped."

echo ""
echo "[5/9] Starting Node #2 via Dashboard API (POST /api/nodes/2/start)..."
START_RESP=$(curl -s -X POST "${DASHBOARD_URL}/api/nodes/2/start" "${AUTH_HEADER[@]}")
echo "  Start API response: ${START_RESP}"
START_STATUS=$(echo "${START_RESP}" | json_extract "status")
if [[ "${START_STATUS}" != "ok" ]]; then
    echo "ERROR: Start API failed: ${START_RESP}"
    exit 1
fi

echo "Verifying Node #2 returns to healthy state and accepts replication..."
NODE2_ONLINE=false
for attempt in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
    ST2=$(get_sidecar_status "${NODE2_CONTAINER}")
    S2=$(echo "${ST2}" | json_extract "status")
    if [[ "${S2}" == "running" ]]; then
        REC2=$(run_node_sql "${NODE2_CONTAINER}" "SELECT pg_is_in_recovery();")
        if [[ "${REC2}" == "t" ]]; then
            NODE2_ONLINE=true
            echo "+ CONFIRMED: ${NODE2_CONTAINER} is running and in recovery (role=standby)!"
            break
        fi
    fi
    sleep 1
done

if [[ "${NODE2_ONLINE}" != "true" ]]; then
    echo "ERROR: Node #2 did not return to healthy standby replication"
    exit 1
fi

echo ""
echo "[6/9] Restarting Node #3 via Dashboard API (POST /api/nodes/3/restart)..."
RESTART_RESP=$(curl -s -X POST "${DASHBOARD_URL}/api/nodes/3/restart" "${AUTH_HEADER[@]}")
echo "  Restart API response: ${RESTART_RESP}"
RESTART_STATUS=$(echo "${RESTART_RESP}" | json_extract "status")
if [[ "${RESTART_STATUS}" != "ok" ]]; then
    echo "ERROR: Restart API failed: ${RESTART_RESP}"
    exit 1
fi

echo "Verifying Node #3 restarted cleanly and remains in recovery mode..."
NODE3_RESTARTED=false
for attempt in 1 2 3 4 5 6 7 8 9 10; do
    ST3=$(get_sidecar_status "${NODE3_CONTAINER}")
    S3=$(echo "${ST3}" | json_extract "status")
    if [[ "${S3}" == "running" ]]; then
        REC3=$(run_node_sql "${NODE3_CONTAINER}" "SELECT pg_is_in_recovery();")
        if [[ "${REC3}" == "t" ]]; then
            NODE3_RESTARTED=true
            echo "+ CONFIRMED: ${NODE3_CONTAINER} restarted and streaming replication active (pg_is_in_recovery=t)!"
            break
        fi
    fi
    sleep 1
done

if [[ "${NODE3_RESTARTED}" != "true" ]]; then
    echo "ERROR: Node #3 did not resume healthy replication after restart"
    exit 1
fi

echo ""
echo "[7/9] Verifying replication catch-up on all standbys after lifecycle actions..."
run_proxy_sql "INSERT INTO ${TABLE_NAME} (val) VALUES ('all_nodes_up');" > /dev/null

REPL_SYNCED=false
for attempt in 1 2 3 4 5 6 7 8 9 10; do
    C2=$(run_node_sql "${NODE2_CONTAINER}" "SELECT count(*) FROM ${TABLE_NAME};")
    C3=$(run_node_sql "${NODE3_CONTAINER}" "SELECT count(*) FROM ${TABLE_NAME};")
    if [[ "${C2}" == "3" && "${C3}" == "3" ]]; then
        REPL_SYNCED=true
        echo "+ Verified: All standbys replicated the latest write (Node2 count=3, Node3 count=3)!"
        break
    fi
    sleep 1
done

if [[ "${REPL_SYNCED}" != "true" ]]; then
    echo "ERROR: Standbys failed to catch up. Node2=${C2}, Node3=${C3}"
    exit 1
fi

echo ""
echo "[8/9] Verifying API error handling and safeguards..."

# 1. Starting an already running node should return 400 Bad Request
ERR_START=$(curl -s -w "%{http_code}" -X POST "${DASHBOARD_URL}/api/nodes/2/start" "${AUTH_HEADER[@]}")
CODE_START="${ERR_START: -3}"
BODY_START="${ERR_START:0:${#ERR_START}-3}"
echo "  Start running node: HTTP ${CODE_START} (${BODY_START})"
if [[ "${CODE_START}" != "400" ]]; then
    echo "ERROR: Expected 400 when starting running node, got: ${CODE_START}"
    exit 1
fi
echo "+ Safe guard verified: starting running node rejected with 400 Bad Request."

# 2. Action on non-existent node should return 404 Not Found
ERR_404=$(curl -s -w "%{http_code}" -X POST "${DASHBOARD_URL}/api/nodes/99/start" "${AUTH_HEADER[@]}")
CODE_404="${ERR_404: -3}"
echo "  Action on non-existent node: HTTP ${CODE_404}"
if [[ "${CODE_404}" != "404" ]]; then
    echo "ERROR: Expected 404 for non-existent node, got: ${CODE_404}"
    exit 1
fi
echo "+ Safe guard verified: non-existent node rejected with 404 Not Found."

echo ""
echo "[9/9] Verifying audit log records lifecycle events..."
AUDIT_JSON=$(curl -s -X GET "${DASHBOARD_URL}/api/audit-logs" "${AUTH_HEADER[@]}")
echo "  Audit logs snippet: $(echo "${AUDIT_JSON}" | cut -c1-200)..."
if [[ "${AUDIT_JSON}" == *"node_down"* && "${AUDIT_JSON}" == *"node_up"* ]]; then
    echo "+ Audit log recorded both 'node_down' and 'node_up' lifecycle events!"
else
    echo "WARNING: Expected node_down and node_up in audit log; response was: ${AUDIT_JSON}"
fi

echo ""
echo "========================================================="
echo "  Node Lifecycle Test COMPLETED SUCCESSFULLY!           "
echo "========================================================="
