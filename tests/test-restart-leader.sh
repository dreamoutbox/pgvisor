#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Stopped Leader Restart & Cluster Rejoin Verification Test
#
# Reproduces and verifies the specific scenario:
# 1. Cluster starts with Node #1 as leader.
# 2. Node #1 is stopped via dashboard API (POST /api/nodes/1/stop).
# 3. Quorum failover promotes a standby (Node #2 or #3) as new leader.
# 4. Writes continue to the new leader via proxy.
# 5. Node #1 is started via dashboard API (POST /api/nodes/1/start).
# 6. Asserts start API succeeds without toast / split-brain crash error:
#    "Postgres not ready after start: Command execution failed: Postgres process exited unexpectedly with status: exit status: 0"
# 7. Node #1 rejoins as standby replica and catches up with replication.
#
# All output is clean plain text (no ANSI escapes).
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
# shellcheck source=tests/lib/cluster.sh
source "${SCRIPT_DIR}/lib/cluster.sh"
# shellcheck source=tests/lib/helper.sh
source "${SCRIPT_DIR}/lib/helper.sh"

readonly TEST_PROXY_PORT=7232
readonly TEST_DASHBOARD_PORT=9880
readonly TEST_MINIO_PORT=10800
readonly TEST_MINIO_CONSOLE=10801
readonly PROJECT_NAME="pgvisor-restart-leader"
readonly COMPOSE_FILE="${REPO_ROOT}/composes/docker-compose.restart-leader.yml"

PROXY_HOST="${PGVISOR_HOST:-localhost}"
PROXY_PORT="${PGVISOR_PORT:-${TEST_PROXY_PORT}}"
DASHBOARD_URL="${PGVISOR_DASHBOARD_URL:-http://localhost:${TEST_DASHBOARD_PORT}}"
TABLE_NAME="t_restart_leader"
ADMIN_TOKEN="${PGVISOR_ADMIN_TOKEN:-postgres}"
AUTH_HEADER=()
if [ -n "${ADMIN_TOKEN}" ]; then
    AUTH_HEADER=(-H "Authorization: Bearer ${ADMIN_TOKEN}")
fi

NODE1_CONTAINER="pgvisor-restart-leader-node1"
NODE2_CONTAINER="pgvisor-restart-leader-node2"
NODE3_CONTAINER="pgvisor-restart-leader-node3"
PROXY_CONTAINER="pgvisor-restart-leader-proxy"

cleanup() {
    echo "Tearing down cluster ${PROJECT_NAME}..."
    cluster_down "${PROJECT_NAME}" "${COMPOSE_FILE}"
}
trap cleanup EXIT

echo "========================================================="
echo "  PgVisor Stopped Leader Restart & Rejoin Test           "
echo "  Project: ${PROJECT_NAME} | Port: ${PROXY_PORT}         "
echo "========================================================="

echo "[0/8] Starting isolated test cluster ${PROJECT_NAME}..."
cluster_down "${PROJECT_NAME}" "${COMPOSE_FILE}"
cluster_up   "${PROJECT_NAME}" "${COMPOSE_FILE}"

echo "Waiting for cluster containers to report healthy..."
wait_for_healthy 120 "${PROJECT_NAME}-minio" "${NODE1_CONTAINER}" "${NODE2_CONTAINER}" "${NODE3_CONTAINER}"
wait_for_proxy_ready "${DASHBOARD_URL}" 60 "${AUTH_HEADER[@]}"

# Helper to execute SQL via PgVisor proxy with retries
run_proxy_sql() {
    local query="$1"
    local output=""
    for attempt in 1 2 3 4 5; do
        if command -v psql &> /dev/null; then
            if output=$(PGPASSWORD="" PGCONNECT_TIMEOUT=5 timeout 15 psql -h "${PROXY_HOST}" -p "${PROXY_PORT}" -U postgres -d postgres -t -A -c "${query}" 2>&1); then
                echo "${output}"
                return 0
            fi
        else
            if output=$(timeout 15 docker exec -i "${PROXY_CONTAINER}" psql -h localhost -p 5432 -U postgres -d postgres -t -A -c "${query}" 2>&1); then
                echo "${output}"
                return 0
            fi
        fi
        sleep 1
    done
    echo "${output}"
    return 1
}


echo ""
echo "[1/8] Verifying initial cluster topology..."
INIT_LEADER=""
for attempt in 1 2 3 4 5 6 7 8 9 10; do
    ST1=$(get_sidecar_status "${NODE1_CONTAINER}")
    R1=$(echo "${ST1}" | json_extract "role")
    S1=$(echo "${ST1}" | json_extract "status")
    if [[ "${R1}" == "leader" && "${S1}" == "running" ]]; then
        INIT_LEADER="1"
        echo "+ Node #1 verified as initial cluster leader."
        break
    fi
    sleep 1
done

if [[ "${INIT_LEADER}" != "1" ]]; then
    echo "ERROR: Node #1 was not elected initial leader. Status: ${ST1}"
    exit 1
fi

echo ""
echo "[2/8] Seeding test table '${TABLE_NAME}' with initial record 'val_before_stop'..."
run_proxy_sql "DROP TABLE IF EXISTS ${TABLE_NAME};" > /dev/null
run_proxy_sql "CREATE TABLE ${TABLE_NAME} (id SERIAL PRIMARY KEY, val TEXT NOT NULL);" > /dev/null
run_proxy_sql "INSERT INTO ${TABLE_NAME} (val) VALUES ('val_before_stop');" > /dev/null
ROW_COUNT=$(run_proxy_sql "SELECT count(*) FROM ${TABLE_NAME};")
if [[ "${ROW_COUNT}" != "1" ]]; then
    echo "ERROR: Expected 1 row in ${TABLE_NAME}, got: ${ROW_COUNT}"
    exit 1
fi
echo "+ Initial data seeded on leader Node #1."

echo ""
echo "[3/8] Stopping leader Node #1 via Dashboard API (POST /api/nodes/1/stop)..."
STOP_RESP=$(curl -s -X POST "${DASHBOARD_URL}/api/nodes/1/stop" "${AUTH_HEADER[@]}")
echo "  Stop API response: ${STOP_RESP}"
STOP_STATUS=$(echo "${STOP_RESP}" | json_extract "status")
if [[ "${STOP_STATUS}" != "ok" ]]; then
    echo "ERROR: Stop API failed on Node #1: ${STOP_RESP}"
    exit 1
fi

echo "Verifying Node #1 reports stopped in sidecar status..."
NODE1_STOPPED=false
for attempt in 1 2 3 4 5 6 7 8 9 10; do
    ST1=$(get_sidecar_status "${NODE1_CONTAINER}")
    S1=$(echo "${ST1}" | json_extract "status")
    if [[ "${S1}" == "stopped" ]]; then
        NODE1_STOPPED=true
        echo "+ CONFIRMED: ${NODE1_CONTAINER} reports status=stopped (child_pid=$(echo "${ST1}" | json_extract "child_pid"))"
        break
    fi
    sleep 1
done

if [[ "${NODE1_STOPPED}" != "true" ]]; then
    echo "ERROR: Node #1 did not report status=stopped. Status: ${ST1}"
    exit 1
fi

echo ""
echo "[4/8] Waiting for standby auto-failover to elect a new leader..."
NEW_LEADER_ID=""
for attempt in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
    ST2=$(get_sidecar_status "${NODE2_CONTAINER}")
    ST3=$(get_sidecar_status "${NODE3_CONTAINER}")
    R2=$(echo "${ST2}" | json_extract "role")
    R3=$(echo "${ST3}" | json_extract "role")
    S2=$(echo "${ST2}" | json_extract "status")
    S3=$(echo "${ST3}" | json_extract "status")

    if [[ "${R2}" == "leader" && "${S2}" == "running" ]]; then
        NEW_LEADER_ID="2"
        echo "+ Standby Node #2 elected as new cluster leader!"
        break
    elif [[ "${R3}" == "leader" && "${S3}" == "running" ]]; then
        NEW_LEADER_ID="3"
        echo "+ Standby Node #3 elected as new cluster leader!"
        break
    fi
    sleep 1
done

if [[ -z "${NEW_LEADER_ID}" ]]; then
    echo "ERROR: Neither Node #2 nor Node #3 was elected leader after Node #1 stopped"
    exit 1
fi

echo ""
echo "[5/8] Inserting write while Node #1 is stopped to verify new leader operation..."
run_proxy_sql "INSERT INTO ${TABLE_NAME} (val) VALUES ('written_while_node1_stopped');" > /dev/null
COUNT_MID=""
for attempt in 1 2 3 4 5; do
    COUNT_MID=$(run_proxy_sql "SELECT count(*) FROM ${TABLE_NAME};")
    if [[ "${COUNT_MID}" == "2" ]]; then
        break
    fi
    sleep 1
done
if [[ "${COUNT_MID}" != "2" ]]; then
    echo "ERROR: Expected 2 rows in ${TABLE_NAME} while Node #1 is stopped, got: ${COUNT_MID}"
    exit 1
fi
echo "+ Write succeeded on new leader Node #${NEW_LEADER_ID}."

echo ""
echo "[6/8] Starting stopped former leader Node #1 via Dashboard API (POST /api/nodes/1/start)..."
START_RESP=$(curl -s -X POST "${DASHBOARD_URL}/api/nodes/1/start" "${AUTH_HEADER[@]}")
echo "  Start API response: ${START_RESP}"
START_STATUS=$(echo "${START_RESP}" | json_extract "status")
if [[ "${START_STATUS}" != "ok" ]]; then
    echo "ERROR: Start API failed on Node #1: ${START_RESP}"
    echo "Failed with unexpected toast error!"
    exit 1
fi
echo "+ CONFIRMED: Start API succeeded without error!"

echo ""
echo "[7/8] Verifying Node #1 safely rejoins cluster as standby replica..."
NODE1_REJOINED=false
for attempt in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
    ST1=$(get_sidecar_status "${NODE1_CONTAINER}")
    R1=$(echo "${ST1}" | json_extract "role")
    S1=$(echo "${ST1}" | json_extract "status")
    P1=$(echo "${ST1}" | json_extract "child_pid")

    if [[ "${S1}" == "running" && "${R1}" == "standby" && "${P1}" -gt 0 ]]; then
        NODE1_REJOINED=true
        echo "+ CONFIRMED: ${NODE1_CONTAINER} running as standby replica (child_pid=${P1})!"
        break
    fi
    sleep 1
done

if [[ "${NODE1_REJOINED}" != "true" ]]; then
    echo "ERROR: Node #1 did not transition to running standby replica. Status: ${ST1}"
    exit 1
fi

echo "Verifying Node #1 is in PostgreSQL recovery mode (standby)..."
IN_REC=$(run_node_sql "${NODE1_CONTAINER}" "SELECT pg_is_in_recovery();")
if [[ "${IN_REC}" != "t" ]]; then
    echo "ERROR: Expected pg_is_in_recovery() == 't' on rejoined Node #1, got: '${IN_REC}'"
    exit 1
fi
echo "+ CONFIRMED: PostgreSQL on Node #1 reports pg_is_in_recovery() = 't'."

echo ""
echo "[8/8] Verifying replication catch-up and ongoing cluster writes..."
echo "Checking that Node #1 replicated rows written while it was stopped..."
NODE1_CAUGHT_UP=false
for attempt in 1 2 3 4 5 6 7 8 9 10; do
    N1_COUNT=$(run_node_sql "${NODE1_CONTAINER}" "SELECT count(*) FROM ${TABLE_NAME};")
    if [[ "${N1_COUNT}" == "2" ]]; then
        NODE1_CAUGHT_UP=true
        echo "+ Verified: Node #1 successfully replicated data written while it was stopped (count=2)!"
        break
    fi
    sleep 1
done

if [[ "${NODE1_CAUGHT_UP}" != "true" ]]; then
    echo "ERROR: Node #1 did not catch up with replication. Count: ${N1_COUNT}"
    exit 1
fi

echo "Inserting additional record 'val_after_node1_started' via proxy..."
run_proxy_sql "INSERT INTO ${TABLE_NAME} (val) VALUES ('val_after_node1_started');" > /dev/null

ALL_NODES_OK=false
for attempt in 1 2 3 4 5 6 7 8 9 10; do
    C1=$(run_node_sql "${NODE1_CONTAINER}" "SELECT count(*) FROM ${TABLE_NAME};")
    C2=$(run_node_sql "${NODE2_CONTAINER}" "SELECT count(*) FROM ${TABLE_NAME};")
    C3=$(run_node_sql "${NODE3_CONTAINER}" "SELECT count(*) FROM ${TABLE_NAME};")
    if [[ "${C1}" == "3" && "${C2}" == "3" && "${C3}" == "3" ]]; then
        ALL_NODES_OK=true
        echo "+ Verified: All 3 nodes have count=3 after restart (Node1=${C1}, Node2=${C2}, Node3=${C3})!"
        break
    fi
    sleep 1
done

if [[ "${ALL_NODES_OK}" != "true" ]]; then
    echo "ERROR: Inconsistent row counts across nodes: Node1=${C1}, Node2=${C2}, Node3=${C3}"
    exit 1
fi

echo "========================================================="
echo "  Restart Leader Test COMPLETED SUCCESSFULLY!            "
echo "========================================================="
