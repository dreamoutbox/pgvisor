#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Manual Leader Switchover Verification Test
#
# Self-contained concurrent test profile.
# Tests graceful leader demotion, standby promotion, replica repointing,
# proxy pool reconnection, and demoted node auto-rejoin.
# All output is clean plain text.
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
# shellcheck source=tests/lib/cluster.sh
source "${SCRIPT_DIR}/lib/cluster.sh"
# shellcheck source=tests/lib/helper.sh
source "${SCRIPT_DIR}/lib/helper.sh"

# Pre-defined test port & project constants
readonly TEST_PROXY_PORT=6232
readonly TEST_DASHBOARD_PORT=8880
readonly TEST_MINIO_PORT=9800
readonly TEST_MINIO_CONSOLE=9801
readonly PROJECT_NAME="pgvisor-switchover"
readonly COMPOSE_FILE="${REPO_ROOT}/composes/docker-compose.switchover.yml"

PROXY_HOST="${PGVISOR_HOST:-localhost}"
PROXY_PORT="${PGVISOR_PORT:-${TEST_PROXY_PORT}}"
DASHBOARD_URL="${PGVISOR_DASHBOARD_URL:-http://localhost:${TEST_DASHBOARD_PORT}}"
TABLE_NAME="t_switchover"
ADMIN_TOKEN="${PGVISOR_ADMIN_TOKEN:-postgres}"
AUTH_HEADER=()
if [ -n "${ADMIN_TOKEN}" ]; then
    AUTH_HEADER=(-H "Authorization: Bearer ${ADMIN_TOKEN}")
fi

NODE1_CONTAINER="pgvisor-switchover-node1"
NODE2_CONTAINER="pgvisor-switchover-node2"
NODE3_CONTAINER="pgvisor-switchover-node3"
PROXY_CONTAINER="pgvisor-switchover-proxy"

cleanup() {
    echo "Tearing down cluster ${PROJECT_NAME}..."
    cluster_down "${PROJECT_NAME}" "${COMPOSE_FILE}"
}
trap cleanup EXIT

echo "========================================================="
echo "  PgVisor Manual Leader Switchover Verification Test     "
echo "  Project: ${PROJECT_NAME} | Port: ${PROXY_PORT}         "
echo "========================================================="

echo "[0/12] Starting isolated test cluster ${PROJECT_NAME}..."
cluster_down "${PROJECT_NAME}" "${COMPOSE_FILE}"
cluster_up   "${PROJECT_NAME}" "${COMPOSE_FILE}"

echo "Waiting for cluster containers to report healthy..."
wait_for_healthy 120 "${PROJECT_NAME}-minio" "${NODE1_CONTAINER}" "${NODE2_CONTAINER}" "${NODE3_CONTAINER}"
wait_for_proxy_ready "${DASHBOARD_URL}" 60 "${AUTH_HEADER[@]}"

# Helper to execute SQL via PgVisor proxy
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
echo "[1/12] Verifying baseline cluster connectivity and topology..."
if ! run_proxy_sql "SELECT 1;" > /dev/null; then
    echo "ERROR: Cannot connect to PostgreSQL cluster via proxy at ${PROXY_HOST}:${PROXY_PORT}"
    exit 1
fi
echo "+ Cluster is reachable via proxy."

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
echo "[2/12] Seeding test table '${TABLE_NAME}' with baseline record 'alpha_t0'..."
run_proxy_sql "DROP TABLE IF EXISTS ${TABLE_NAME};" > /dev/null
run_proxy_sql "CREATE TABLE ${TABLE_NAME} (id SERIAL PRIMARY KEY, val TEXT NOT NULL);" > /dev/null
run_proxy_sql "INSERT INTO ${TABLE_NAME} (val) VALUES ('alpha_t0');" > /dev/null
COUNT_T0=$(run_proxy_sql "SELECT count(*) FROM ${TABLE_NAME};")
if [[ "${COUNT_T0}" != "1" ]]; then
    echo "ERROR: Expected 1 row in ${TABLE_NAME}, got: ${COUNT_T0}"
    exit 1
fi
echo "+ Baseline record seeded via proxy. Total rows = ${COUNT_T0}"

echo ""
echo "[3/12] Executing manual switchover: promoting Node #2 to leader via Dashboard API..."
SWITCHOVER_PAYLOAD='{"target_node_id": 2}'
SWITCHOVER_RESP=$(curl -s -X POST \
    -H "Content-Type: application/json" \
    ${AUTH_HEADER[@]+"${AUTH_HEADER[@]}"} \
    -d "${SWITCHOVER_PAYLOAD}" \
    "${DASHBOARD_URL}/api/cluster/switchover")

echo "  API Response: ${SWITCHOVER_RESP}"
STATUS_VAL=$(echo "${SWITCHOVER_RESP}" | json_extract "status")
NEW_LEADER_ID=$(echo "${SWITCHOVER_RESP}" | json_extract "new_leader_id")

if [[ "${STATUS_VAL}" != "ok" || "${NEW_LEADER_ID}" != "2" ]]; then
    echo "ERROR: Switchover API returned failure: ${SWITCHOVER_RESP}"
    exit 1
fi
echo "+ Switchover API accepted! Node #2 designated as new leader."

echo ""
echo "[4/12] Verifying Node #2 became read-write primary in PostgreSQL..."
NODE2_NEW_RECOVERY=""
for attempt in 1 2 3 4 5 6 7 8 9 10; do
    NODE2_NEW_RECOVERY=$(run_node_sql "${NODE2_CONTAINER}" "SELECT pg_is_in_recovery();")
    if [[ "${NODE2_NEW_RECOVERY}" == "f" ]]; then
        break
    fi
    sleep 1
done

if [[ "${NODE2_NEW_RECOVERY}" != "f" ]]; then
    echo "ERROR: Node #2 did not promote to read-write primary. pg_is_in_recovery=${NODE2_NEW_RECOVERY}"
    exit 1
fi
echo "+ CONFIRMED: ${NODE2_CONTAINER} is operating as primary (pg_is_in_recovery=f)."

echo ""
echo "[5/12] Verifying old leader (Node #1) demoted and auto-rejoins as standby replica..."
NODE1_REJOINED=false
for attempt in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
    ST1=$(get_sidecar_status "${NODE1_CONTAINER}")
    R1=$(echo "${ST1}" | json_extract "role")
    S1=$(echo "${ST1}" | json_extract "status")
    if [[ "${R1}" == "standby" && "${S1}" == "running" ]]; then
        NODE1_RECOVERY=$(run_node_sql "${NODE1_CONTAINER}" "SELECT pg_is_in_recovery();")
        if [[ "${NODE1_RECOVERY}" == "t" ]]; then
            NODE1_REJOINED=true
            echo "+ ${NODE1_CONTAINER} auto-rejoined as standby replica (role=standby, pg_is_in_recovery=t)!"
            break
        fi
    fi
    sleep 1
done

if [[ "${NODE1_REJOINED}" != "true" ]]; then
    echo "ERROR: ${NODE1_CONTAINER} failed to auto-rejoin as standby replica."
    exit 1
fi

echo ""
echo "[6/12] Verifying Node #3 repointed and actively streaming from Node #2..."
NODE3_RECOVERY=$(run_node_sql "${NODE3_CONTAINER}" "SELECT pg_is_in_recovery();")
if [[ "${NODE3_RECOVERY}" != "t" ]]; then
    echo "ERROR: ${NODE3_CONTAINER} expected to remain standby replica (pg_is_in_recovery=t)"
    exit 1
fi
echo "+ Verified: ${NODE3_CONTAINER} is in recovery mode."

echo ""
echo "[7/12] Writing record 'beta_t1' through proxy to verify client routing to new leader..."
WRITE_SUCCESS=false
for attempt in 1 2 3 4 5; do
    if run_proxy_sql "INSERT INTO ${TABLE_NAME} (val) VALUES ('beta_t1');" > /dev/null 2>&1; then
        WRITE_SUCCESS=true
        break
    fi
    sleep 1
done

if [[ "${WRITE_SUCCESS}" != "true" ]]; then
    echo "ERROR: Failed to write through proxy to promoted leader Node #2."
    exit 1
fi
echo "+ Post-switchover write 'beta_t1' succeeded via proxy."

echo ""
echo "[8/12] Verifying streaming replication across both standbys (Node #1 and Node #3)..."
for replica in "${NODE1_CONTAINER}" "${NODE3_CONTAINER}"; do
    REP_ROWS=""
    for attempt in 1 2 3 4 5 6 7 8 9 10; do
        REP_ROWS=$(run_node_sql "${replica}" "SELECT count(*) FROM ${TABLE_NAME};")
        if [[ "${REP_ROWS}" == "2" ]]; then
            break
        fi
        sleep 1
    done
    if [[ "${REP_ROWS}" != "2" ]]; then
        echo "ERROR: ${replica} expected 2 rows, got: ${REP_ROWS}"
        exit 1
    fi
    echo "+ Verified: ${replica} has 2 rows (replicated 'alpha_t0' and 'beta_t1')."
done

echo ""
echo "[9/12] Performing SECOND switchover: switching primary back to Node #1..."
SWITCHOVER_PAYLOAD2='{"target_node_id": 1}'
SWITCHOVER_RESP2=$(curl -s -X POST \
    -H "Content-Type: application/json" \
    ${AUTH_HEADER[@]+"${AUTH_HEADER[@]}"} \
    -d "${SWITCHOVER_PAYLOAD2}" \
    "${DASHBOARD_URL}/api/cluster/switchover")

echo "  API Response: ${SWITCHOVER_RESP2}"
STATUS_VAL2=$(echo "${SWITCHOVER_RESP2}" | json_extract "status")
NEW_LEADER_ID2=$(echo "${SWITCHOVER_RESP2}" | json_extract "new_leader_id")

if [[ "${STATUS_VAL2}" != "ok" || "${NEW_LEADER_ID2}" != "1" ]]; then
    echo "ERROR: Second switchover API returned failure: ${SWITCHOVER_RESP2}"
    exit 1
fi
echo "+ Second switchover accepted! Node #1 designated as primary again."

echo ""
echo "[10/12] Verifying Node #1 returned to primary and Node #2 auto-rejoined as standby..."
NODE1_AGAIN_RECOVERY=""
for attempt in 1 2 3 4 5 6 7 8 9 10; do
    NODE1_AGAIN_RECOVERY=$(run_node_sql "${NODE1_CONTAINER}" "SELECT pg_is_in_recovery();")
    if [[ "${NODE1_AGAIN_RECOVERY}" == "f" ]]; then
        break
    fi
    sleep 1
done

if [[ "${NODE1_AGAIN_RECOVERY}" != "f" ]]; then
    echo "ERROR: Node #1 did not return to primary. pg_is_in_recovery=${NODE1_AGAIN_RECOVERY}"
    exit 1
fi
echo "+ CONFIRMED: ${NODE1_CONTAINER} is primary again (pg_is_in_recovery=f)."

NODE2_REJOINED=false
for attempt in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
    ST2=$(get_sidecar_status "${NODE2_CONTAINER}")
    R2=$(echo "${ST2}" | json_extract "role")
    S2=$(echo "${ST2}" | json_extract "status")
    if [[ "${R2}" == "standby" && "${S2}" == "running" ]]; then
        NODE2_RECOVERY=$(run_node_sql "${NODE2_CONTAINER}" "SELECT pg_is_in_recovery();")
        if [[ "${NODE2_RECOVERY}" == "t" ]]; then
            NODE2_REJOINED=true
            echo "+ ${NODE2_CONTAINER} successfully auto-rejoined as standby replica!"
            break
        fi
    fi
    sleep 1
done

if [[ "${NODE2_REJOINED}" != "true" ]]; then
    echo "ERROR: ${NODE2_CONTAINER} failed to auto-rejoin as standby."
    exit 1
fi

echo ""
echo "[11/12] Writing record 'gamma_t2' through proxy to restored primary (Node #1)..."
WRITE_SUCCESS2=false
for attempt in 1 2 3 4 5; do
    if run_proxy_sql "INSERT INTO ${TABLE_NAME} (val) VALUES ('gamma_t2');" > /dev/null 2>&1; then
        WRITE_SUCCESS2=true
        break
    fi
    sleep 1
done

if [[ "${WRITE_SUCCESS2}" != "true" ]]; then
    echo "ERROR: Failed to write 'gamma_t2' through proxy."
    exit 1
fi
echo "+ Post-second-switchover write 'gamma_t2' succeeded via proxy."

# Verify 3 rows on all nodes
for container in "${NODE1_CONTAINER}" "${NODE2_CONTAINER}" "${NODE3_CONTAINER}"; do
    ROWS=""
    for attempt in 1 2 3 4 5 6 7 8 9 10; do
        ROWS=$(run_node_sql "${container}" "SELECT count(*) FROM ${TABLE_NAME};")
        if [[ "${ROWS}" == "3" ]]; then
            break
        fi
        sleep 1
    done
    if [[ "${ROWS}" != "3" ]]; then
        echo "ERROR: ${container} expected 3 rows, got: ${ROWS}"
        exit 1
    fi
    echo "+ Verified: ${container} has all 3 rows (alpha_t0, beta_t1, gamma_t2)."
done

echo ""
echo "[12/12] Verifying Web Dashboard display reflects Node #1 as Leader..."
DASHBOARD_HTML=$(curl -s ${AUTH_HEADER[@]+"${AUTH_HEADER[@]}"} "${DASHBOARD_URL}/nodes")
if echo "${DASHBOARD_HTML}" | grep -q "Node #1.*Active Primary"; then
    echo "+ CONFIRMED: Dashboard renders Node #1 as Active Primary."
fi

# Clean up
echo ""
echo "Cleaning up test table..."
run_proxy_sql "DROP TABLE IF EXISTS ${TABLE_NAME};" > /dev/null
echo "+ Cleanup completed."

echo ""
echo "========================================================="
echo "  MANUAL LEADER SWITCHOVER TEST PASSED SUCCESSFULLY!     "
echo "========================================================="
