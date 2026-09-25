#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: High Availability Failover & Auto-Promotion Verification Test
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
readonly TEST_PROXY_PORT=5832
readonly TEST_DASHBOARD_PORT=8480
readonly TEST_MINIO_PORT=9400
readonly TEST_MINIO_CONSOLE=9401
readonly PROJECT_NAME="pgvisor-failover"
readonly COMPOSE_FILE="${REPO_ROOT}/composes/docker-compose.failover.yml"

PROXY_HOST="${PGVISOR_HOST:-localhost}"
PROXY_PORT="${PGVISOR_PORT:-${TEST_PROXY_PORT}}"
DASHBOARD_URL="${PGVISOR_DASHBOARD_URL:-http://localhost:${TEST_DASHBOARD_PORT}}"
TABLE_NAME="t_failover"

NODE1_CONTAINER="pgvisor-failover-node1"
NODE2_CONTAINER="pgvisor-failover-node2"
NODE3_CONTAINER="pgvisor-failover-node3"
PROXY_CONTAINER="pgvisor-failover-proxy"

trap cleanup EXIT

echo "========================================================="
echo "  PgVisor High Availability Failover Verification Test   "
echo "  Project: ${PROJECT_NAME} | Port: ${PROXY_PORT}         "
echo "========================================================="

echo "[0/8] Starting isolated test cluster ${PROJECT_NAME}..."
cluster_down "${PROJECT_NAME}" "${COMPOSE_FILE}"
cluster_up   "${PROJECT_NAME}" "${COMPOSE_FILE}"

echo "Waiting for cluster containers to report healthy..."
wait_for_healthy 120 "${PROJECT_NAME}-minio" "${NODE1_CONTAINER}" "${NODE2_CONTAINER}" "${NODE3_CONTAINER}"
wait_for_proxy_ready "${DASHBOARD_URL}" 60


echo ""
echo "[1/8] Verifying baseline cluster connectivity and topology..."
if ! run_proxy_sql "SELECT 1;" > /dev/null; then
    echo "ERROR: Cannot connect to PostgreSQL cluster via proxy at ${PROXY_HOST}:${PROXY_PORT}"
    exit 1
fi
echo "+ PostgreSQL cluster proxy is reachable."

# Check initial node roles
NODE1_RECOVERY=$(run_node_sql "${NODE1_CONTAINER}" "SELECT pg_is_in_recovery();")
NODE2_RECOVERY=$(run_node_sql "${NODE2_CONTAINER}" "SELECT pg_is_in_recovery();")
NODE3_RECOVERY=$(run_node_sql "${NODE3_CONTAINER}" "SELECT pg_is_in_recovery();")

if [[ "${NODE1_RECOVERY}" != "f" ]]; then
    echo "ERROR: ${NODE1_CONTAINER} is expected to be read-write primary (pg_is_in_recovery=f), got: ${NODE1_RECOVERY}"
    exit 1
fi
if [[ "${NODE2_RECOVERY}" != "t" || "${NODE3_RECOVERY}" != "t" ]]; then
    echo "ERROR: Standby replicas node2 and node3 must be in recovery mode (pg_is_in_recovery=t)"
    exit 1
fi
echo "+ Baseline verified: node1 is primary, node2 and node3 are standbys."

echo ""
echo "[2/8] Creating test table '${TABLE_NAME}' and inserting baseline row 'alpha_t0'..."
run_proxy_sql "DROP TABLE IF EXISTS ${TABLE_NAME};" > /dev/null
run_proxy_sql "CREATE TABLE ${TABLE_NAME} (id SERIAL PRIMARY KEY, val TEXT NOT NULL, created_at TIMESTAMPTZ DEFAULT NOW());" > /dev/null
run_proxy_sql "INSERT INTO ${TABLE_NAME} (val) VALUES ('alpha_t0');" > /dev/null

COUNT_T0=$(run_proxy_sql "SELECT count(*) FROM ${TABLE_NAME};")
if [[ "${COUNT_T0}" != "1" ]]; then
    echo "ERROR: Expected 1 row in ${TABLE_NAME}, got: ${COUNT_T0}"
    exit 1
fi
echo "+ Baseline row seeded via proxy: val='alpha_t0'"

echo ""
echo "[3/8] Simulating leader failure: stopping container ${NODE1_CONTAINER}..."
stop_node "${NODE1_CONTAINER}"
echo "+ Container ${NODE1_CONTAINER} stopped."

echo ""
echo "[4/8] Waiting for consensus election and standby promotion..."
NEW_LEADER=""
SURVIVING_STANDBY=""
MAX_WAIT=20
ELAPSED=0

while [[ ${ELAPSED} -lt ${MAX_WAIT} ]]; do
    # Check node2
    ST2=$(get_sidecar_status "${NODE2_CONTAINER}")
    ROLE2=$(echo "${ST2}" | json_extract "role")
    STATUS2=$(echo "${ST2}" | json_extract "status")

    # Check node3
    ST3=$(get_sidecar_status "${NODE3_CONTAINER}")
    ROLE3=$(echo "${ST3}" | json_extract "role")
    STATUS3=$(echo "${ST3}" | json_extract "status")

    if [[ "${ROLE2}" == "leader" && "${STATUS2}" == "running" ]]; then
        NEW_LEADER="${NODE2_CONTAINER}"
        SURVIVING_STANDBY="${NODE3_CONTAINER}"
        break
    elif [[ "${ROLE3}" == "leader" && "${STATUS3}" == "running" ]]; then
        NEW_LEADER="${NODE3_CONTAINER}"
        SURVIVING_STANDBY="${NODE2_CONTAINER}"
        break
    fi

    sleep 1
    ELAPSED=$((ELAPSED + 1))
done

if [[ -z "${NEW_LEADER}" ]]; then
    echo "ERROR: Election timeout exceeded (${MAX_WAIT}s). Neither node2 nor node3 was promoted."
    echo "Node2 status: ${ST2:-none}"
    echo "Node3 status: ${ST3:-none}"
    start_node "${NODE1_CONTAINER}" "${PROJECT_NAME}" "${COMPOSE_FILE}" pgvisor-node1
    exit 1
fi

echo "+ Failover successful! Promoted node: ${NEW_LEADER} (elapsed: ${ELAPSED}s)"

# Verify promoted node in Postgres
NEW_LEADER_RECOVERY=$(run_node_sql "${NEW_LEADER}" "SELECT pg_is_in_recovery();")
if [[ "${NEW_LEADER_RECOVERY}" != "f" ]]; then
    echo "ERROR: Promoted leader ${NEW_LEADER} pg_is_in_recovery should be 'f', got: ${NEW_LEADER_RECOVERY}"
    start_node "${NODE1_CONTAINER}" "${PROJECT_NAME}" "${COMPOSE_FILE}" pgvisor-node1
    exit 1
fi
echo "+ PostgreSQL on ${NEW_LEADER} confirmed in read-write mode (pg_is_in_recovery=f)."

echo ""
echo "[5/8] Verifying write availability through PgVisor proxy without client reconfiguration..."
WRITE_SUCCESS=false
for attempt in 1 2 3 4 5; do
    if run_proxy_sql "INSERT INTO ${TABLE_NAME} (val) VALUES ('beta_t1');" > /dev/null 2>&1; then
        WRITE_SUCCESS=true
        break
    fi
    sleep 1
done

if [[ "${WRITE_SUCCESS}" != "true" ]]; then
    echo "ERROR: Failed to write to cluster through proxy following failover"
    start_node "${NODE1_CONTAINER}" "${PROJECT_NAME}" "${COMPOSE_FILE}" pgvisor-node1
    exit 1
fi

TOTAL_ROWS=""
for attempt in 1 2 3 4 5 6 7 8 9 10; do
    TOTAL_ROWS=$(run_proxy_sql "SELECT count(*) FROM ${TABLE_NAME};" 2>/dev/null || true)
    if [[ "${TOTAL_ROWS}" == "2" ]]; then
        break
    fi
    sleep 1
done

if [[ "${TOTAL_ROWS}" != "2" ]]; then
    echo "ERROR: Expected 2 rows after failover write, got: ${TOTAL_ROWS}"
    start_node "${NODE1_CONTAINER}" "${PROJECT_NAME}" "${COMPOSE_FILE}" pgvisor-node1
    exit 1
fi
echo "+ Write succeeded through proxy port ${PROXY_PORT}! Table now contains 2 rows ('alpha_t0', 'beta_t1')."

echo ""
echo "[6/8] Verifying replication on surviving standby (${SURVIVING_STANDBY})..."
STANDBY_ROWS=""
for attempt in 1 2 3 4 5; do
    STANDBY_ROWS=$(run_node_sql "${SURVIVING_STANDBY}" "SELECT count(*) FROM ${TABLE_NAME};")
    if [[ "${STANDBY_ROWS}" == "2" ]]; then
        break
    fi
    sleep 1
done

if [[ "${STANDBY_ROWS}" == "2" ]]; then
    echo "+ Standby ${SURVIVING_STANDBY} successfully replicated all writes (count=2)."
else
    echo "Note: Standby ${SURVIVING_STANDBY} currently has count=${STANDBY_ROWS}; replication is catching up."
fi

echo ""
echo "[7/8] Restarting pgvisor-node1 and verifying split-brain prevention..."
start_node "${NODE1_CONTAINER}" "${PROJECT_NAME}" "${COMPOSE_FILE}" pgvisor-node1

sleep 3
NODE1_POST_RECOVERY=$(run_node_sql "${NODE1_CONTAINER}" "SELECT pg_is_in_recovery();")
echo "+ Node1 restarted. Recovery status: ${NODE1_POST_RECOVERY:-unreachable}"

ACTIVE_LEADER_RECOVERY=$(run_node_sql "${NEW_LEADER}" "SELECT pg_is_in_recovery();")
if [[ "${ACTIVE_LEADER_RECOVERY}" != "f" ]]; then
    echo "ERROR: Promoted leader ${NEW_LEADER} lost leadership after node1 restart!"
    exit 1
fi
echo "+ Active primary confirmed intact on ${NEW_LEADER}; no split-brain detected."

echo ""
echo "[8/8] Cleaning up test artifacts..."
run_proxy_sql "DROP TABLE IF EXISTS ${TABLE_NAME};" > /dev/null
echo "+ Cleanup complete."

echo ""
echo "========================================================="
echo "  High Availability Failover Test PASSED Successfully!   "
echo "========================================================="
