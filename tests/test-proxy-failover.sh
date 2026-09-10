#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Dual-Proxy High Availability & Redundancy Verification Test
#
# Tests a 2-proxy, 3-node cluster topology.
# Stops proxy1 and asserts that PostgreSQL remains fully accessible for both
# reads and writes through proxy2, followed by proxy1 recovery verification.
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
# shellcheck source=tests/lib/cluster.sh
source "${SCRIPT_DIR}/lib/cluster.sh"

# Pre-defined test port & project constants
readonly TEST_PROXY1_PORT=6832
readonly TEST_DASHBOARD1_PORT=9480
readonly TEST_PROXY2_PORT=6833
readonly TEST_DASHBOARD2_PORT=9481
readonly TEST_MINIO_PORT=10400
readonly TEST_MINIO_CONSOLE=10401
readonly PROJECT_NAME="pgvisor-proxy-failover"
readonly COMPOSE_BASE="${REPO_ROOT}/composes/docker-compose.proxy-failover.yml"
readonly COMPOSE_PROXY2="${REPO_ROOT}/composes/docker-compose.proxy-failover-proxy2.yml"

PROXY1_PORT="${PGVISOR_PROXY1_PORT:-${TEST_PROXY1_PORT}}"
PROXY2_PORT="${PGVISOR_PROXY2_PORT:-${TEST_PROXY2_PORT}}"
DASHBOARD1_URL="http://localhost:${TEST_DASHBOARD1_PORT}"
DASHBOARD2_URL="http://localhost:${TEST_DASHBOARD2_PORT}"
TABLE_NAME="t_proxy_failover"

ADMIN_TOKEN="${PGVISOR_ADMIN_TOKEN:-postgres}"
AUTH_HEADER=()
if [ -n "${ADMIN_TOKEN}" ]; then
    AUTH_HEADER=(-H "Authorization: Bearer ${ADMIN_TOKEN}")
fi

NODE1_CONTAINER="pgvisor-proxy-failover-node1"
NODE2_CONTAINER="pgvisor-proxy-failover-node2"
NODE3_CONTAINER="pgvisor-proxy-failover-node3"
PROXY1_CONTAINER="pgvisor-proxy-failover-proxy"
PROXY2_CONTAINER="pgvisor-proxy-failover-proxy2"
MINIO_CONTAINER="pgvisor-proxy-failover-minio"

cleanup() {
    echo ""
    echo "Tearing down cluster ${PROJECT_NAME}..."
    cluster_down "${PROJECT_NAME}" "${COMPOSE_BASE}" -f "${COMPOSE_PROXY2}"
    echo "+ Cleanup complete."
}
trap cleanup EXIT

echo "========================================================="
echo "  PgVisor Dual-Proxy Redundancy Verification Test        "
echo "  Project: ${PROJECT_NAME}                               "
echo "  Proxy 1: port ${PROXY1_PORT} (dash: ${TEST_DASHBOARD1_PORT})             "
echo "  Proxy 2: port ${PROXY2_PORT} (dash: ${TEST_DASHBOARD2_PORT})             "
echo "========================================================="

echo ""
echo "[0/6] Starting isolated cluster with 2 proxies and 3 nodes..."
cluster_down "${PROJECT_NAME}" "${COMPOSE_BASE}" -f "${COMPOSE_PROXY2}"
cluster_up   "${PROJECT_NAME}" "${COMPOSE_BASE}" -f "${COMPOSE_PROXY2}"

echo "Waiting for cluster containers to report healthy..."
wait_for_healthy 120 "${MINIO_CONTAINER}" "${NODE1_CONTAINER}" "${NODE2_CONTAINER}" "${NODE3_CONTAINER}"
echo "+ Database nodes and storage are healthy."

echo "Waiting for proxy1 and proxy2 dashboards to be ready..."
wait_for_proxy_ready "${DASHBOARD1_URL}" 60 "${AUTH_HEADER[@]}"
wait_for_proxy_ready "${DASHBOARD2_URL}" 60 "${AUTH_HEADER[@]}"
echo "+ Both proxy1 and proxy2 are ready and serving traffic."

# Helper to execute SQL queries via proxy with retry loop for replication lag
run_proxy_sql() {
    local port="$1"
    local container="$2"
    local query="$3"
    local max_attempts="${4:-10}"
    local output=""
    for attempt in $(seq 1 "${max_attempts}"); do
        if command -v psql &> /dev/null; then
            if output=$(PGPASSWORD="" PGCONNECT_TIMEOUT=5 timeout 15 psql -h localhost -p "${port}" -U postgres -d postgres -t -A -c "${query}" 2>&1); then
                echo "${output}"
                return 0
            fi
        else
            if output=$(timeout 15 docker exec -i "${container}" psql -h localhost -p 5432 -U postgres -d postgres -t -A -c "${query}" 2>&1); then
                echo "${output}"
                return 0
            fi
        fi
        sleep 1
    done
    echo "${output}"
    return 1
}

run_proxy1_sql() {
    run_proxy_sql "${PROXY1_PORT}" "${PROXY1_CONTAINER}" "$1" "${2:-10}"
}

run_proxy2_sql() {
    run_proxy_sql "${PROXY2_PORT}" "${PROXY2_CONTAINER}" "$1" "${2:-10}"
}

run_node_sql() {
    local container="$1"
    local query="$2"
    docker exec -i "${container}" psql -U postgres -d postgres -t -A -c "${query}" 2>/dev/null || true
}

echo ""
echo "[1/6] Verifying baseline connectivity and cluster topology..."
if ! run_proxy1_sql "SELECT 1;" > /dev/null; then
    echo "ERROR: Cannot connect to PostgreSQL via proxy1 at port ${PROXY1_PORT}"
    exit 1
fi
echo "+ Connected via proxy1 (port ${PROXY1_PORT})."

if ! run_proxy2_sql "SELECT 1;" > /dev/null; then
    echo "ERROR: Cannot connect to PostgreSQL via proxy2 at port ${PROXY2_PORT}"
    exit 1
fi
echo "+ Connected via proxy2 (port ${PROXY2_PORT})."

NODE1_RECOVERY=$(run_node_sql "${NODE1_CONTAINER}" "SELECT pg_is_in_recovery();")
NODE2_RECOVERY=$(run_node_sql "${NODE2_CONTAINER}" "SELECT pg_is_in_recovery();")
NODE3_RECOVERY=$(run_node_sql "${NODE3_CONTAINER}" "SELECT pg_is_in_recovery();")

if [[ "${NODE1_RECOVERY}" != "f" ]]; then
    echo "ERROR: ${NODE1_CONTAINER} expected to be primary (pg_is_in_recovery=f), got: ${NODE1_RECOVERY}"
    exit 1
fi
if [[ "${NODE2_RECOVERY}" != "t" || "${NODE3_RECOVERY}" != "t" ]]; then
    echo "ERROR: node2 and node3 must be standbys (pg_is_in_recovery=t)"
    exit 1
fi
echo "+ Baseline cluster verified: node1 is primary, node2 and node3 are standbys."

echo ""
echo "[2/6] Creating test table via proxy1 and verifying read consistency across both proxies..."
run_proxy1_sql "DROP TABLE IF EXISTS ${TABLE_NAME};" > /dev/null
run_proxy1_sql "CREATE TABLE ${TABLE_NAME} (id SERIAL PRIMARY KEY, val TEXT NOT NULL, created_at TIMESTAMPTZ DEFAULT NOW());" > /dev/null
run_proxy1_sql "INSERT INTO ${TABLE_NAME} (val) VALUES ('from_proxy1');" > /dev/null

COUNT_P1=$(run_proxy1_sql "SELECT count(*) FROM ${TABLE_NAME};")
if [[ "${COUNT_P1}" != "1" ]]; then
    echo "ERROR: Expected 1 row in ${TABLE_NAME} via proxy1, got: ${COUNT_P1}"
    exit 1
fi
echo "+ Written 1 row via proxy1 ('from_proxy1')."

# Read via proxy2 with retry loop to accommodate replication lag
COUNT_P2=""
for attempt in $(seq 1 10); do
    COUNT_P2=$(run_proxy2_sql "SELECT count(*) FROM ${TABLE_NAME};" 1 || echo "")
    if [[ "${COUNT_P2}" == "1" ]]; then
        break
    fi
    sleep 1
done

if [[ "${COUNT_P2}" != "1" ]]; then
    echo "ERROR: Expected 1 row in ${TABLE_NAME} via proxy2, got: ${COUNT_P2}"
    exit 1
fi
echo "+ Read verified via proxy2: row count = 1."

echo ""
echo "[3/6] Stopping proxy1 container (${PROXY1_CONTAINER})..."
stop_node "${PROXY1_CONTAINER}"

# Verify proxy1 is down: dashboard and SQL must fail
PROXY1_DOWN=false
if ! curl -s -m 2 "${DASHBOARD1_URL}/api/status" > /dev/null 2>&1; then
    PROXY1_DOWN=true
fi

if [[ "${PROXY1_DOWN}" != "true" ]]; then
    echo "ERROR: proxy1 dashboard still reachable after stopping ${PROXY1_CONTAINER}"
    exit 1
fi

if command -v psql &> /dev/null; then
    if PGPASSWORD="" PGCONNECT_TIMEOUT=2 timeout 3 psql -h localhost -p "${PROXY1_PORT}" -U postgres -d postgres -t -A -c "SELECT 1;" > /dev/null 2>&1; then
        echo "ERROR: proxy1 SQL listener unexpectedly responded after container stopped!"
        exit 1
    fi
fi
echo "+ Confirmed proxy1 (${PROXY1_CONTAINER}) is completely stopped."

echo ""
echo "[4/6] Asserting full database access via proxy2 while proxy1 is stopped..."

# Write mutating transaction through proxy2 to verify write routing to leader
WRITE_SUCCESS=false
for attempt in $(seq 1 5); do
    if run_proxy2_sql "INSERT INTO ${TABLE_NAME} (val) VALUES ('from_proxy2_while_proxy1_down');" > /dev/null 2>&1; then
        WRITE_SUCCESS=true
        break
    fi
    sleep 1
done

if [[ "${WRITE_SUCCESS}" != "true" ]]; then
    echo "ERROR: Failed to write to database through proxy2 while proxy1 is down!"
    exit 1
fi
echo "+ Write succeeded through proxy2 while proxy1 is offline."

# Verify row count via proxy2 (retry loop for standby read routing)
TOTAL_P2=""
for attempt in $(seq 1 10); do
    TOTAL_P2=$(run_proxy2_sql "SELECT count(*) FROM ${TABLE_NAME};" 1 || echo "")
    if [[ "${TOTAL_P2}" == "2" ]]; then
        break
    fi
    sleep 1
done

if [[ "${TOTAL_P2}" != "2" ]]; then
    echo "ERROR: Expected 2 rows in ${TABLE_NAME} via proxy2, got: ${TOTAL_P2}"
    exit 1
fi
echo "+ Read count verified via proxy2: 2 rows."

# Verify all data values are intact through proxy2
VALS=$(run_proxy2_sql "SELECT val FROM ${TABLE_NAME} ORDER BY id;" | tr '\n' ',' | sed 's/,$//')
if [[ "${VALS}" != "from_proxy1,from_proxy2_while_proxy1_down" ]]; then
    echo "ERROR: Unexpected data via proxy2: ${VALS}"
    exit 1
fi
echo "+ Data integrity verified via proxy2: [${VALS}]."

echo ""
echo "[5/6] Restarting proxy1 and verifying multi-proxy cluster recovery..."
start_node "${PROXY1_CONTAINER}" "${PROJECT_NAME}" "${COMPOSE_BASE}" pgvisor-proxy

echo "Waiting for restarted proxy1 dashboard to be ready..."
wait_for_proxy_ready "${DASHBOARD1_URL}" 60 "${AUTH_HEADER[@]}"
echo "+ Proxy1 is back online."

# Verify proxy1 sees existing data
COUNT_P1_RECOVERED=""
for attempt in $(seq 1 10); do
    COUNT_P1_RECOVERED=$(run_proxy1_sql "SELECT count(*) FROM ${TABLE_NAME};" 1 || echo "")
    if [[ "${COUNT_P1_RECOVERED}" == "2" ]]; then
        break
    fi
    sleep 1
done

if [[ "${COUNT_P1_RECOVERED}" != "2" ]]; then
    echo "ERROR: Restarted proxy1 sees unexpected row count: ${COUNT_P1_RECOVERED}"
    exit 1
fi
echo "+ Restarted proxy1 sees all 2 rows."

# Write via proxy1 and verify via proxy2
run_proxy1_sql "INSERT INTO ${TABLE_NAME} (val) VALUES ('from_proxy1_after_recovery');" > /dev/null

COUNT_FINAL=""
for attempt in $(seq 1 10); do
    COUNT_FINAL=$(run_proxy2_sql "SELECT count(*) FROM ${TABLE_NAME};" 1 || echo "")
    if [[ "${COUNT_FINAL}" == "3" ]]; then
        break
    fi
    sleep 1
done

if [[ "${COUNT_FINAL}" != "3" ]]; then
    echo "ERROR: Expected 3 rows via proxy2 after proxy1 recovery write, got: ${COUNT_FINAL}"
    exit 1
fi
echo "+ Cross-proxy write/read verified: 3 rows present across both proxies."

echo ""
echo "[6/6] Cleaning up test table..."
run_proxy2_sql "DROP TABLE IF EXISTS ${TABLE_NAME};" > /dev/null
echo "+ Table ${TABLE_NAME} cleaned up."

echo ""
echo "========================================================="
echo "  Dual-Proxy Redundancy Verification Test PASSED!        "
echo "========================================================="
