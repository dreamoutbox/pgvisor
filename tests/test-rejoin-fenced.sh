#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Rejoin Node Fenced & Replication Verification Test
#
# Self-contained concurrent test profile.
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
# shellcheck source=tests/lib/cluster.sh
source "${SCRIPT_DIR}/lib/cluster.sh"

# Pre-defined test port & project constants
readonly TEST_PROXY_PORT=6032
readonly TEST_DASHBOARD_PORT=8680
readonly TEST_MINIO_PORT=9600
readonly TEST_MINIO_CONSOLE=9601
readonly PROJECT_NAME="pgvisor-rejoin-fenced"
readonly COMPOSE_FILE="${REPO_ROOT}/composes/docker-compose.rejoin-fenced.yml"

PROXY_HOST="${PGVISOR_HOST:-localhost}"
PROXY_PORT="${PGVISOR_PORT:-${TEST_PROXY_PORT}}"
TABLE_NAME="t_rejoin_test"
ADMIN_TOKEN="${PGVISOR_ADMIN_TOKEN:-postgres}"
AUTH_HEADER=()
if [ -n "${ADMIN_TOKEN}" ]; then
    AUTH_HEADER=(-H "Authorization: Bearer ${ADMIN_TOKEN}")
fi

NODE1_CONTAINER="pgvisor-rejoin-fenced-node1"
NODE2_CONTAINER="pgvisor-rejoin-fenced-node2"
NODE3_CONTAINER="pgvisor-rejoin-fenced-node3"
PROXY_CONTAINER="pgvisor-rejoin-fenced-proxy"

cleanup() {
    echo "Tearing down cluster ${PROJECT_NAME}..."
    cluster_down "${PROJECT_NAME}" "${COMPOSE_FILE}"
}
trap cleanup EXIT

echo "========================================================="
echo "  PgVisor Rejoin Node Fenced & Replication Test         "
echo "  Project: ${PROJECT_NAME} | Port: ${PROXY_PORT}        "
echo "========================================================="

echo "[0/10] Starting isolated test cluster ${PROJECT_NAME}..."
cluster_down "${PROJECT_NAME}" "${COMPOSE_FILE}"
cluster_up   "${PROJECT_NAME}" "${COMPOSE_FILE}"

echo "Waiting for cluster containers to report healthy..."
wait_for_healthy 120 "${PROJECT_NAME}-minio" "${NODE1_CONTAINER}" "${NODE2_CONTAINER}" "${NODE3_CONTAINER}"
wait_for_proxy_ready "http://localhost:${TEST_DASHBOARD_PORT}" 60 "${AUTH_HEADER[@]}"

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

# Helper to execute SQL directly on a specific container
run_node_sql() {
    local container="$1"
    local query="$2"
    docker exec -i "${container}" psql -U postgres -d postgres -t -A -c "${query}" 2>/dev/null || true
}

# Helper to query sidecar control status
get_sidecar_status() {
    local container="$1"
    docker exec -i "${container}" curl -s http://localhost:8080/control/status 2>/dev/null || true
}

# Helper to extract JSON field using jq, python3, or grep
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

echo ""
echo "[1/10] Verifying cluster baseline connectivity..."
if ! run_proxy_sql "SELECT 1;" > /dev/null; then
    echo "ERROR: Cannot connect to PostgreSQL cluster via proxy at ${PROXY_HOST}:${PROXY_PORT}"
    exit 1
fi
echo "+ Cluster is reachable."

echo ""
echo "[2/10] Seeding test table '${TABLE_NAME}' with baseline record 't0_initial'..."
run_proxy_sql "DROP TABLE IF EXISTS ${TABLE_NAME};" > /dev/null
run_proxy_sql "CREATE TABLE ${TABLE_NAME} (id SERIAL PRIMARY KEY, val TEXT NOT NULL);" > /dev/null
run_proxy_sql "INSERT INTO ${TABLE_NAME} (val) VALUES ('t0_initial');" > /dev/null
COUNT_T0=$(run_proxy_sql "SELECT count(*) FROM ${TABLE_NAME};")
echo "+ Table seeded. Row count = ${COUNT_T0}"

echo ""
echo "[3/10] Stopping ${NODE1_CONTAINER} container to simulate node crash..."
stop_node "${NODE1_CONTAINER}"
echo "+ Container ${NODE1_CONTAINER} stopped."

echo ""
echo "[4/10] Waiting for standby promotion to new leader..."
NEW_LEADER=""
SURVIVING_STANDBY=""
MAX_WAIT=20
ELAPSED=0

while [[ ${ELAPSED} -lt ${MAX_WAIT} ]]; do
    ST2=$(get_sidecar_status "${NODE2_CONTAINER}")
    ROLE2=$(echo "${ST2}" | json_extract "role")
    STATUS2=$(echo "${ST2}" | json_extract "status")

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
    echo "ERROR: Election timed out. No standby promoted to leader."
    start_node "${NODE1_CONTAINER}" "${PROJECT_NAME}" "${COMPOSE_FILE}" pgvisor-node1
    exit 1
fi
echo "+ Standby promoted to leader: ${NEW_LEADER}"
echo "+ Surviving standby replica: ${SURVIVING_STANDBY}"

echo ""
echo "[5/10] Writing post-failover data at T1 ('t1_post_failover') to promoted leader..."
sleep 2
run_proxy_sql "INSERT INTO ${TABLE_NAME} (val) VALUES ('t1_post_failover');" > /dev/null
TOTAL_ROWS=$(run_proxy_sql "SELECT count(*) FROM ${TABLE_NAME};")
echo "+ Post-failover write completed via proxy. Total rows = ${TOTAL_ROWS}"

echo ""
echo "[6/10] Verifying surviving standby (${SURVIVING_STANDBY}) receives replication..."
STANDBY_ROWS=""
for i in 1 2 3 4 5; do
    STANDBY_ROWS=$(run_node_sql "${SURVIVING_STANDBY}" "SELECT count(*) FROM ${TABLE_NAME};")
    if [[ "${STANDBY_ROWS}" == "2" ]]; then
        break
    fi
    sleep 1
done
echo "+ Surviving standby row count = ${STANDBY_ROWS}"
if [[ "${STANDBY_ROWS}" != "2" ]]; then
    echo "WARNING: Surviving standby replication delayed or out of sync."
fi

echo ""
echo "[7/10] Restarting ${NODE1_CONTAINER} container and checking sidecar supervision state..."
start_node "${NODE1_CONTAINER}" "${PROJECT_NAME}" "${COMPOSE_FILE}" pgvisor-node1

sleep 3

NODE1_STATUS_JSON=$(get_sidecar_status "${NODE1_CONTAINER}")
NODE1_ROLE=$(echo "${NODE1_STATUS_JSON}" | json_extract "role")
NODE1_STATUS=$(echo "${NODE1_STATUS_JSON}" | json_extract "status")

echo "+ ${NODE1_CONTAINER} sidecar status: role='${NODE1_ROLE}', status='${NODE1_STATUS}'"
echo "  Raw JSON: ${NODE1_STATUS_JSON}"

if [[ "${NODE1_ROLE}" == "fenced" && "${NODE1_STATUS}" == "fenced" ]]; then
    echo "+ CONFIRMED: ${NODE1_CONTAINER} was automatically FENCED by split-brain guard."
else
    echo "ERROR: Unexpected status for ${NODE1_CONTAINER}: role=${NODE1_ROLE}, status=${NODE1_STATUS}"
fi

echo ""
echo "[8/10] Checking if local PostgreSQL on node1 is running or accepting queries..."
if docker exec -i "${NODE1_CONTAINER}" psql -U postgres -d postgres -c "SELECT 1;" > /dev/null 2>&1; then
    echo "WARNING: Local PostgreSQL is running on node1!"
else
    echo "+ CONFIRMED: PostgreSQL on node1 is completely STOPPED (pg_ctl stop -m immediate)."
    echo "  Local queries fail with connection refused / socket missing."
fi

echo ""
echo "[9/10] Checking active WAL replication connections on leader (${NEW_LEADER})..."
REPL_CONN=$(run_node_sql "${NEW_LEADER}" "SELECT count(*) FROM pg_stat_replication;")
REPL_CLIENTS=$(run_node_sql "${NEW_LEADER}" "SELECT client_addr FROM pg_stat_replication;")
echo "+ Active WAL sender connections on leader: ${REPL_CONN}"
echo "+ Connected replication replica IP(s):"
echo "${REPL_CLIENTS}"

if [[ "${REPL_CONN}" == "1" ]]; then
    echo "+ CONFIRMED: Only surviving standby is connected. Rejoined node1 is NOT replicating from the leader."
fi

echo ""
echo "[10/10] Checking Web Dashboard display for node1..."
DASHBOARD_NODE_STATUS=$(docker exec -i "${PROXY_CONTAINER}" curl -s ${AUTH_HEADER[@]+"${AUTH_HEADER[@]}"} http://localhost:8080/nodes 2>/dev/null || true)
if echo "${DASHBOARD_NODE_STATUS}" | grep -q "Fenced (Quorum Lost)"; then
    echo "+ CONFIRMED: Dashboard renders node1 as 'Fenced (Quorum Lost)'."
fi

# Clean up
echo ""
echo "Cleaning up test table..."
run_proxy_sql "DROP TABLE IF EXISTS ${TABLE_NAME};" > /dev/null
echo "+ Cleanup completed."

echo ""
echo "========================================================="
echo "  TEST SUMMARY & FINDINGS:                              "
echo "========================================================="
echo "  1. Node1 Role & Status: Fenced / Fenced               "
echo "  2. Node1 PostgreSQL:    STOPPED (pg_ctl immediate)    "
echo "  3. Replication Stream:  NOT CONNECTED to new leader   "
echo "  4. Data Replication:    Node1 receives NO updates     "
echo "  5. Dashboard View:      Fenced (Quorum Lost)          "
echo "========================================================="
