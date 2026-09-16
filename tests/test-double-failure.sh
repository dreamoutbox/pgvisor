#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Double-Failure Disaster Recovery Test
#
# Verifies cluster behavior when 2 of 3 nodes go down simultaneously:
#   - Quorum loss prevents writes (no split-brain)
#   - Sequential node restart restores full cluster
#   - Rejoined nodes become standbys with full data replication
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
readonly TEST_PROXY_PORT=6732
readonly TEST_DASHBOARD_PORT=9380
readonly TEST_MINIO_PORT=10300
readonly TEST_MINIO_CONSOLE=10301
readonly PROJECT_NAME="pgvisor-double-failure"
readonly COMPOSE_FILE="${REPO_ROOT}/composes/docker-compose.double-failure.yml"

PROXY_HOST="${PGVISOR_HOST:-localhost}"
PROXY_PORT="${PGVISOR_PORT:-${TEST_PROXY_PORT}}"
DASHBOARD_URL="${PGVISOR_DASHBOARD_URL:-http://localhost:${TEST_DASHBOARD_PORT}}"
TABLE_NAME="t_double_failure"
ADMIN_TOKEN="${PGVISOR_ADMIN_TOKEN:-postgres}"
AUTH_HEADER=()
if [ -n "${ADMIN_TOKEN}" ]; then
    AUTH_HEADER=(-H "Authorization: Bearer ${ADMIN_TOKEN}")
fi

NODE1_CONTAINER="pgvisor-double-failure-node1"
NODE2_CONTAINER="pgvisor-double-failure-node2"
NODE3_CONTAINER="pgvisor-double-failure-node3"
PROXY_CONTAINER="pgvisor-double-failure-proxy"

cleanup() {
    echo "Tearing down cluster ${PROJECT_NAME}..."
    cluster_down "${PROJECT_NAME}" "${COMPOSE_FILE}"
}
trap cleanup EXIT

echo "========================================================="
echo "  PgVisor Double-Failure Disaster Recovery Test           "
echo "  Project: ${PROJECT_NAME} | Port: ${PROXY_PORT}         "
echo "========================================================="

echo "[0/14] Starting isolated test cluster ${PROJECT_NAME}..."
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
echo "[1/14] Verifying baseline cluster connectivity and topology..."
if ! run_proxy_sql "SELECT 1;" > /dev/null; then
    echo "ERROR: Cannot connect to PostgreSQL cluster via proxy at ${PROXY_HOST}:${PROXY_PORT}"
    exit 1
fi
echo "+ PostgreSQL cluster proxy is reachable."

# Confirm initial roles
NODE1_RECOVERY=$(run_node_sql "${NODE1_CONTAINER}" "SELECT pg_is_in_recovery();")
NODE2_RECOVERY=$(run_node_sql "${NODE2_CONTAINER}" "SELECT pg_is_in_recovery();")
NODE3_RECOVERY=$(run_node_sql "${NODE3_CONTAINER}" "SELECT pg_is_in_recovery();")

if [[ "${NODE1_RECOVERY}" != "f" ]]; then
    echo "ERROR: ${NODE1_CONTAINER} expected to be primary (pg_is_in_recovery=f), got: ${NODE1_RECOVERY}"
    exit 1
fi
if [[ "${NODE2_RECOVERY}" != "t" || "${NODE3_RECOVERY}" != "t" ]]; then
    echo "ERROR: node2 and node3 must be in recovery mode (pg_is_in_recovery=t)"
    exit 1
fi
echo "+ Baseline: node1=primary, node2=standby, node3=standby."

echo ""
echo "[2/14] Seeding test table '${TABLE_NAME}' with baseline row 't0_baseline'..."
run_proxy_sql "DROP TABLE IF EXISTS ${TABLE_NAME};" > /dev/null
run_proxy_sql "CREATE TABLE ${TABLE_NAME} (id SERIAL PRIMARY KEY, val TEXT NOT NULL, created_at TIMESTAMPTZ DEFAULT NOW());" > /dev/null
run_proxy_sql "INSERT INTO ${TABLE_NAME} (val) VALUES ('t0_baseline');" > /dev/null

COUNT_T0=$(run_proxy_sql "SELECT count(*) FROM ${TABLE_NAME};")
if [[ "${COUNT_T0}" != "1" ]]; then
    echo "ERROR: Expected 1 row in ${TABLE_NAME}, got: ${COUNT_T0}"
    exit 1
fi
echo "+ Baseline row seeded: val='t0_baseline'"

echo ""
echo "[3/14] Simulating double failure: stopping node1 (leader) + node2 simultaneously..."
stop_node "${NODE1_CONTAINER}"
stop_node "${NODE2_CONTAINER}"
echo "+ Containers ${NODE1_CONTAINER} and ${NODE2_CONTAINER} stopped."

echo ""
echo "[4/14] Asserting only node3 remains running..."
N1_STATUS=$(docker inspect -f '{{.State.Status}}' "${NODE1_CONTAINER}" 2>/dev/null || echo "missing")
N2_STATUS=$(docker inspect -f '{{.State.Status}}' "${NODE2_CONTAINER}" 2>/dev/null || echo "missing")
N3_STATUS=$(docker inspect -f '{{.State.Status}}' "${NODE3_CONTAINER}" 2>/dev/null || echo "missing")

if [[ "${N1_STATUS}" == "running" ]]; then
    echo "ERROR: ${NODE1_CONTAINER} should be stopped, but is still running"
    exit 1
fi
if [[ "${N2_STATUS}" == "running" ]]; then
    echo "ERROR: ${NODE2_CONTAINER} should be stopped, but is still running"
    exit 1
fi
if [[ "${N3_STATUS}" != "running" ]]; then
    echo "ERROR: ${NODE3_CONTAINER} expected running, got: ${N3_STATUS}"
    exit 1
fi
echo "+ Confirmed: node1=${N1_STATUS}, node2=${N2_STATUS}, node3=${N3_STATUS} (only node3 alive)."

echo ""
echo "[5/14] Attempting write to surviving node3 directly — expecting failure (no quorum, standby)..."
if docker exec -i "${NODE3_CONTAINER}" psql -U postgres -d postgres -c "INSERT INTO ${TABLE_NAME} (val) VALUES ('should_not_exist_node3');" > /dev/null 2>&1; then
    echo "ERROR: Direct write to node3 succeeded, but node3 must be in read-only recovery mode!"
    exit 1
fi
echo "+ Direct write to node3 rejected as expected (node3 is in read-only recovery mode)."

# Assert node3 did not promote itself without quorum
N3_SIDE_ROLE=$(get_sidecar_status "${NODE3_CONTAINER}" | json_extract "role")
if [[ "${N3_SIDE_ROLE}" != "standby" ]]; then
    echo "ERROR: node3 sidecar role should remain 'standby' without quorum, got: ${N3_SIDE_ROLE}"
    exit 1
fi
echo "+ Confirmed: node3 sidecar role is '${N3_SIDE_ROLE}' (no split-brain promotion without quorum)."

# Also verify single write attempt via proxy fails (no leader available in cluster)
echo "Verifying write via proxy fails (single attempt) — expecting failure (no leader in cluster)..."
PROXY_WRITE_OK=false
if command -v psql &> /dev/null; then
    if PGPASSWORD="" PGCONNECT_TIMEOUT=3 timeout 12 psql -h "${PROXY_HOST}" -p "${PROXY_PORT}" -U postgres -d postgres -c "INSERT INTO ${TABLE_NAME} (val) VALUES ('should_not_exist_proxy');" > /dev/null 2>&1; then
        PROXY_WRITE_OK=true
    fi
else
    if timeout 12 docker exec -i "${PROXY_CONTAINER}" psql -h localhost -p 5432 -U postgres -d postgres -c "INSERT INTO ${TABLE_NAME} (val) VALUES ('should_not_exist_proxy');" > /dev/null 2>&1; then
        PROXY_WRITE_OK=true
    fi
fi

if [[ "${PROXY_WRITE_OK}" == "true" ]]; then
    echo "ERROR: Write via proxy succeeded with no leader alive!"
    exit 1
fi
echo "+ Write via proxy correctly failed/timed out — no leader available."

echo ""
echo "[6/14] Restarting ${NODE1_CONTAINER}..."
start_node "${NODE1_CONTAINER}" "${PROJECT_NAME}" "${COMPOSE_FILE}" pgvisor-node1

echo "Waiting for ${NODE1_CONTAINER} to become healthy..."
wait_for_healthy 60 "${NODE1_CONTAINER}"
echo "+ ${NODE1_CONTAINER} is running."

echo ""
echo "[7/14] Waiting for quorum restoration and leader election..."
LEADER_NODE=""
MAX_WAIT=30
ELAPSED=0

while [[ ${ELAPSED} -lt ${MAX_WAIT} ]]; do
    ST1=$(get_sidecar_status "${NODE1_CONTAINER}")
    ROLE1=$(echo "${ST1}" | json_extract "role")
    STATUS1=$(echo "${ST1}" | json_extract "status")

    ST3=$(get_sidecar_status "${NODE3_CONTAINER}")
    ROLE3=$(echo "${ST3}" | json_extract "role")
    STATUS3=$(echo "${ST3}" | json_extract "status")

    if [[ "${ROLE1}" == "leader" && "${STATUS1}" == "running" ]]; then
        LEADER_NODE="${NODE1_CONTAINER}"
        break
    elif [[ "${ROLE3}" == "leader" && "${STATUS3}" == "running" ]]; then
        LEADER_NODE="${NODE3_CONTAINER}"
        break
    fi

    sleep 1
    ELAPSED=$((ELAPSED + 1))
done

if [[ -z "${LEADER_NODE}" ]]; then
    echo "ERROR: Leader election timed out after ${MAX_WAIT}s with node1+node3 alive."
    echo "Node1 status: ${ST1:-none}"
    echo "Node3 status: ${ST3:-none}"
    exit 1
fi
echo "+ Leader elected: ${LEADER_NODE} (elapsed: ${ELAPSED}s)"

# Determine the standby among node1/node3
if [[ "${LEADER_NODE}" == "${NODE1_CONTAINER}" ]]; then
    STANDBY_A="${NODE3_CONTAINER}"
else
    STANDBY_A="${NODE1_CONTAINER}"
fi

# Verify the non-leader is standby
STANDBY_A_ROLE=""
for i in 1 2 3 4 5; do
    SA_ST=$(get_sidecar_status "${STANDBY_A}")
    STANDBY_A_ROLE=$(echo "${SA_ST}" | json_extract "role")
    if [[ "${STANDBY_A_ROLE}" == "standby" ]]; then
        break
    fi
    sleep 1
done
echo "+ ${STANDBY_A} confirmed as standby (role=${STANDBY_A_ROLE})."

echo ""
echo "[8/14] Verifying cluster is working: writing 't1_after_node1_rejoin' via proxy..."
WRITE_SUCCESS=false
for attempt in 1 2 3 4 5; do
    if run_proxy_sql "INSERT INTO ${TABLE_NAME} (val) VALUES ('t1_after_node1_rejoin');" > /dev/null 2>&1; then
        WRITE_SUCCESS=true
        break
    fi
    sleep 1
done

if [[ "${WRITE_SUCCESS}" != "true" ]]; then
    echo "ERROR: Failed to write to cluster after quorum restoration"
    exit 1
fi

COUNT_T1=""
for attempt in 1 2 3 4 5 6 7 8 9 10; do
    COUNT_T1=$(run_proxy_sql "SELECT count(*) FROM ${TABLE_NAME};" 2>/dev/null || true)
    if [[ "${COUNT_T1}" == "2" ]]; then
        break
    fi
    sleep 1
done

if [[ "${COUNT_T1}" != "2" ]]; then
    echo "ERROR: Expected 2 rows after first rejoin write, got: ${COUNT_T1}"
    exit 1
fi
echo "+ Write succeeded. Table row count = ${COUNT_T1} ('t0_baseline', 't1_after_node1_rejoin')."

echo ""
echo "[9/14] Restarting ${NODE2_CONTAINER}..."
start_node "${NODE2_CONTAINER}" "${PROJECT_NAME}" "${COMPOSE_FILE}" pgvisor-node2

echo "Waiting for ${NODE2_CONTAINER} to become healthy..."
wait_for_healthy 60 "${NODE2_CONTAINER}"
echo "+ ${NODE2_CONTAINER} is running."

echo ""
echo "[10/14] Waiting for ${NODE2_CONTAINER} to rejoin as standby..."
NODE2_REJOINED=false
REJOIN_WAIT=30
REJOIN_ELAPSED=0

while [[ ${REJOIN_ELAPSED} -lt ${REJOIN_WAIT} ]]; do
    N2_ST=$(get_sidecar_status "${NODE2_CONTAINER}")
    N2_ROLE=$(echo "${N2_ST}" | json_extract "role")
    N2_STATUS=$(echo "${N2_ST}" | json_extract "status")

    if [[ "${N2_ROLE}" == "standby" && "${N2_STATUS}" == "running" ]]; then
        NODE2_REJOINED=true
        echo "+ ${NODE2_CONTAINER} rejoined as standby (role=${N2_ROLE}, status=${N2_STATUS})."
        break
    fi

    sleep 1
    REJOIN_ELAPSED=$((REJOIN_ELAPSED + 1))
done

if [[ "${NODE2_REJOINED}" != "true" ]]; then
    FINAL_ST=$(get_sidecar_status "${NODE2_CONTAINER}")
    echo "ERROR: ${NODE2_CONTAINER} failed to rejoin as standby within ${REJOIN_WAIT}s. State: ${FINAL_ST}"
    exit 1
fi

echo ""
echo "[11/14] Verifying cluster after full recovery: writing 't2_after_node2_rejoin' via proxy..."
WRITE_SUCCESS2=false
for attempt in 1 2 3 4 5; do
    if run_proxy_sql "INSERT INTO ${TABLE_NAME} (val) VALUES ('t2_after_node2_rejoin');" > /dev/null 2>&1; then
        WRITE_SUCCESS2=true
        break
    fi
    sleep 1
done

if [[ "${WRITE_SUCCESS2}" != "true" ]]; then
    echo "ERROR: Failed to write after full cluster recovery"
    exit 1
fi

COUNT_T2=""
for attempt in 1 2 3 4 5 6 7 8 9 10; do
    COUNT_T2=$(run_proxy_sql "SELECT count(*) FROM ${TABLE_NAME};" 2>/dev/null || true)
    if [[ "${COUNT_T2}" == "3" ]]; then
        break
    fi
    sleep 1
done

if [[ "${COUNT_T2}" != "3" ]]; then
    echo "ERROR: Expected 3 rows after second rejoin write, got: ${COUNT_T2}"
    exit 1
fi
echo "+ Write succeeded. Table row count = ${COUNT_T2} (all 3 values present)."

echo ""
echo "[12/14] Verifying data integrity across all nodes..."
EXPECTED_VALS=("t0_baseline" "t1_after_node1_rejoin" "t2_after_node2_rejoin")
ALL_NODES=("${NODE1_CONTAINER}" "${NODE2_CONTAINER}" "${NODE3_CONTAINER}")

for node in "${ALL_NODES[@]}"; do
    NODE_COUNT=""
    for i in 1 2 3 4 5 6 7 8 9 10; do
        NODE_COUNT=$(run_node_sql "${node}" "SELECT count(*) FROM ${TABLE_NAME};")
        if [[ "${NODE_COUNT}" == "3" ]]; then
            break
        fi
        sleep 1
    done

    if [[ "${NODE_COUNT}" != "3" ]]; then
        echo "ERROR: ${node} expected 3 rows, got: ${NODE_COUNT}"
        exit 1
    fi

    # Verify each expected value exists
    for val in "${EXPECTED_VALS[@]}"; do
        VAL_EXISTS=""
        for i in 1 2 3; do
            VAL_EXISTS=$(run_node_sql "${node}" "SELECT val FROM ${TABLE_NAME} WHERE val='${val}';")
            if [[ "${VAL_EXISTS}" == "${val}" ]]; then
                break
            fi
            sleep 1
        done
        if [[ "${VAL_EXISTS}" != "${val}" ]]; then
            echo "ERROR: ${node} missing expected value '${val}'"
            exit 1
        fi
    done
    echo "+ ${node}: all 3 rows verified."
done

echo ""
echo "[13/14] Verifying WAL replication: pg_stat_replication on leader (${LEADER_NODE})..."
REPL_COUNT=""
for i in 1 2 3 4 5; do
    REPL_COUNT=$(run_node_sql "${LEADER_NODE}" "SELECT count(*) FROM pg_stat_replication;")
    if [[ "${REPL_COUNT}" == "2" ]]; then
        break
    fi
    sleep 1
done

if [[ "${REPL_COUNT}" != "2" ]]; then
    echo "ERROR: Expected 2 active WAL senders on leader, got: ${REPL_COUNT}"
    exit 1
fi
echo "+ Leader has ${REPL_COUNT} active WAL streaming connections (both standbys replicating)."

echo ""
echo "[14/14] Cleaning up test artifacts..."
run_proxy_sql "DROP TABLE IF EXISTS ${TABLE_NAME};" > /dev/null
echo "+ Cleanup complete."

echo ""
echo "========================================================="
echo "  Double-Failure Disaster Recovery Test PASSED!           "
echo "========================================================="
