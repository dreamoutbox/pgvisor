#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Scale Out / Add New Node Verification Test
#
# Verifies:
#   1. Baseline 3-node cluster connectivity and replication topology.
#   2. Seed test table with pre-scale record ('t0_pre_scale') via proxy.
#   3. Launch new standby node 'pgvisor-node4' dynamically.
#   4. Verify pgvisor-node4 sidecar status (role="standby", status="running").
#   5. Verify pgvisor-node4 PostgreSQL recovery state (pg_is_in_recovery=t).
#   6. Verify pg_basebackup cloned historical data ('t0_pre_scale' present).
#   7. Verify leader pg_stat_replication increased to 3 active WAL sender streams.
#   8. Write post-scale record ('t1_post_scale') through proxy.
#   9. Verify pgvisor-node4 receives and applies live streaming replication.
#  10. Verify cluster consensus stability across heartbeat cycles.
#  11. Clean up pgvisor-node4 container and volume.
# ==============================================================================

PROXY_HOST="${PGVISOR_HOST:-localhost}"
PROXY_PORT="${PGVISOR_PORT:-5432}"
DASHBOARD_URL="${PGVISOR_DASHBOARD_URL:-http://localhost:8080}"
TABLE_NAME="t_scale"
ADMIN_TOKEN="${PGVISOR_ADMIN_TOKEN:-postgres}"
AUTH_HEADER=()
if [ -n "${ADMIN_TOKEN}" ]; then
    AUTH_HEADER=(-H "Authorization: Bearer ${ADMIN_TOKEN}")
fi

echo "========================================================="
echo "  PgVisor Scale Out: Add New Node Verification Test      "
echo "========================================================="

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
            if output=$(timeout 15 docker compose exec -T pgvisor-proxy psql -h localhost -p 5432 -U postgres -d postgres -t -A -c "${query}" 2>&1); then
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
    docker compose exec -T "${container}" psql -U postgres -d postgres -t -A -c "${query}" 2>/dev/null || true
}

# Helper to query sidecar control status
get_sidecar_status() {
    local container="$1"
    docker compose exec -T "${container}" curl -s http://localhost:8080/control/status 2>/dev/null || true
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

COMPOSE_BASE="docker-compose.yml"
COMPOSE_NODE4="composes/docker-compose.add-node4.yml"

cleanup() {
    echo ""
    echo "Cleaning up pgvisor-node4 container, volume, and test table..."
    docker compose -f "${COMPOSE_BASE}" -f "${COMPOSE_NODE4}" stop pgvisor-node4 > /dev/null 2>&1 || true
    docker compose -f "${COMPOSE_BASE}" -f "${COMPOSE_NODE4}" rm -f -v pgvisor-node4 > /dev/null 2>&1 || true
    docker volume rm pgvisor_node4_data > /dev/null 2>&1 || true
    run_proxy_sql "DROP TABLE IF EXISTS ${TABLE_NAME};" > /dev/null 2>&1 || true
    echo "+ Cleanup complete."
}
trap cleanup EXIT

echo ""
echo "[1/10] Verifying baseline cluster connectivity and replication topology..."
if ! run_proxy_sql "SELECT 1;" > /dev/null; then
    echo "ERROR: Cannot connect to PostgreSQL cluster via proxy at ${PROXY_HOST}:${PROXY_PORT}"
    exit 1
fi
echo "+ Cluster is reachable via proxy."

# Detect active leader among baseline nodes
ACTIVE_LEADER=""
for node in pgvisor-node1 pgvisor-node2 pgvisor-node3; do
    role=$(get_sidecar_status "${node}" | json_extract "role")
    if [[ "${role}" == "leader" ]]; then
        ACTIVE_LEADER="${node}"
        break
    fi
done

if [[ -z "${ACTIVE_LEADER}" ]]; then
    echo "ERROR: Could not detect active leader among baseline nodes"
    exit 1
fi
echo "+ Active cluster leader: ${ACTIVE_LEADER}"

INITIAL_WAL_SENDERS=$(run_node_sql "${ACTIVE_LEADER}" "SELECT count(*) FROM pg_stat_replication;")
echo "+ Baseline active replication connections on leader: ${INITIAL_WAL_SENDERS}"
if [[ "${INITIAL_WAL_SENDERS}" -lt 2 ]]; then
    echo "ERROR: Expected at least 2 standbys connected to leader, got: ${INITIAL_WAL_SENDERS}"
    exit 1
fi

echo ""
echo "[2/10] Seeding test table '${TABLE_NAME}' with baseline record 't0_pre_scale'..."
run_proxy_sql "DROP TABLE IF EXISTS ${TABLE_NAME};" > /dev/null
run_proxy_sql "CREATE TABLE ${TABLE_NAME} (id SERIAL PRIMARY KEY, val TEXT NOT NULL, created_at TIMESTAMPTZ DEFAULT NOW());" > /dev/null
run_proxy_sql "INSERT INTO ${TABLE_NAME} (val) VALUES ('t0_pre_scale');" > /dev/null

COUNT_T0=$(run_proxy_sql "SELECT count(*) FROM ${TABLE_NAME};")
if [[ "${COUNT_T0}" != "1" ]]; then
    echo "ERROR: Expected 1 row in ${TABLE_NAME}, got: ${COUNT_T0}"
    exit 1
fi
echo "+ Baseline row seeded via proxy: val='t0_pre_scale'"

echo ""
echo "[3/10] Dynamically launching new standby node: pgvisor-node4..."
export PRIMARY_CONNINFO="host=${ACTIVE_LEADER} port=5432 user=postgres"
docker compose -f "${COMPOSE_BASE}" -f "${COMPOSE_NODE4}" up -d pgvisor-node4

echo "Waiting for pgvisor-node4 container to report healthy..."
MAX_WAIT=30
ELAPSED=0
NODE4_HEALTHY=false
while [[ ${ELAPSED} -lt ${MAX_WAIT} ]]; do
    HEALTH_STATUS=$(docker inspect -f '{{.State.Health.Status}}' pgvisor-node4 2>/dev/null || echo "starting")
    if [[ "${HEALTH_STATUS}" == "healthy" ]]; then
        NODE4_HEALTHY=true
        break
    fi
    sleep 1
    ELAPSED=$((ELAPSED + 1))
done

if [[ "${NODE4_HEALTHY}" != "true" ]]; then
    echo "ERROR: pgvisor-node4 did not become healthy within ${MAX_WAIT}s"
    docker compose -f "${COMPOSE_BASE}" -f "${COMPOSE_NODE4}" logs pgvisor-node4
    exit 1
fi
echo "+ pgvisor-node4 container is healthy (elapsed: ${ELAPSED}s)."

echo ""
echo "[4/10] Verifying pgvisor-node4 sidecar status and role..."
ST4=$(get_sidecar_status "pgvisor-node4")
ROLE4=$(echo "${ST4}" | json_extract "role")
STATUS4=$(echo "${ST4}" | json_extract "status")

echo "+ pgvisor-node4 sidecar reports: role='${ROLE4}', status='${STATUS4}'"
if [[ "${ROLE4}" != "standby" || "${STATUS4}" != "running" ]]; then
    echo "ERROR: Unexpected status for pgvisor-node4: role='${ROLE4}', status='${STATUS4}'"
    exit 1
fi

echo ""
echo "[5/10] Verifying PostgreSQL recovery mode and historical data clone on pgvisor-node4..."
NODE4_RECOVERY=$(run_node_sql "pgvisor-node4" "SELECT pg_is_in_recovery();")
if [[ "${NODE4_RECOVERY}" != "t" ]]; then
    echo "ERROR: pgvisor-node4 PostgreSQL is not in recovery mode (expected 't', got '${NODE4_RECOVERY}')"
    exit 1
fi
echo "+ PostgreSQL on pgvisor-node4 is running in hot standby recovery mode (pg_is_in_recovery=t)."

HISTORICAL_COUNT=$(run_node_sql "pgvisor-node4" "SELECT count(*) FROM ${TABLE_NAME} WHERE val = 't0_pre_scale';")
if [[ "${HISTORICAL_COUNT}" != "1" ]]; then
    echo "ERROR: Historical record 't0_pre_scale' missing on pgvisor-node4 (got count=${HISTORICAL_COUNT})"
    exit 1
fi
echo "+ Historical baseline record cloned successfully via pg_basebackup (count=${HISTORICAL_COUNT})."

echo ""
echo "[6/10] Verifying leader pg_stat_replication includes pgvisor-node4..."
UPDATED_WAL_SENDERS=$(run_node_sql "${ACTIVE_LEADER}" "SELECT count(*) FROM pg_stat_replication;")
echo "+ Active replication streams on leader: ${UPDATED_WAL_SENDERS}"
if [[ "${UPDATED_WAL_SENDERS}" -le "${INITIAL_WAL_SENDERS}" ]]; then
    echo "ERROR: WAL sender count did not increase after node4 joined (before: ${INITIAL_WAL_SENDERS}, after: ${UPDATED_WAL_SENDERS})"
    exit 1
fi
echo "+ Leader now actively streams WAL to 3 connected standby replicas."

echo ""
echo "[7/10] Writing post-scale data ('t1_post_scale') through proxy..."
run_proxy_sql "INSERT INTO ${TABLE_NAME} (val) VALUES ('t1_post_scale');" > /dev/null
TOTAL_ROWS=$(run_proxy_sql "SELECT count(*) FROM ${TABLE_NAME};")
if [[ "${TOTAL_ROWS}" != "2" ]]; then
    echo "ERROR: Expected 2 rows in ${TABLE_NAME} via proxy, got: ${TOTAL_ROWS}"
    exit 1
fi
echo "+ Write succeeded through proxy. Table row count = ${TOTAL_ROWS}."

echo ""
echo "[8/10] Verifying live streaming replication to pgvisor-node4..."
REPLICATED=false
for attempt in 1 2 3 4 5; do
    NODE4_ROWS=$(run_node_sql "pgvisor-node4" "SELECT count(*) FROM ${TABLE_NAME};")
    if [[ "${NODE4_ROWS}" == "2" ]]; then
        REPLICATED=true
        break
    fi
    sleep 1
done

if [[ "${REPLICATED}" != "true" ]]; then
    echo "ERROR: pgvisor-node4 did not receive post-scale write (count=${NODE4_ROWS})"
    exit 1
fi
echo "+ Live streaming replication verified on pgvisor-node4 (row count = 2)."

echo ""
echo "[9/10] Verifying cluster stability and absence of spurious elections..."
sleep 3
ST4_STABLE=$(get_sidecar_status "pgvisor-node4")
ROLE4_STABLE=$(echo "${ST4_STABLE}" | json_extract "role")
STATUS4_STABLE=$(echo "${ST4_STABLE}" | json_extract "status")

if [[ "${ROLE4_STABLE}" != "standby" || "${STATUS4_STABLE}" != "running" ]]; then
    echo "ERROR: pgvisor-node4 became unstable: role='${ROLE4_STABLE}', status='${STATUS4_STABLE}'"
    exit 1
fi

LEADER_CHECK=$(run_node_sql "${ACTIVE_LEADER}" "SELECT pg_is_in_recovery();")
if [[ "${LEADER_CHECK}" != "f" ]]; then
    echo "ERROR: Leader ${ACTIVE_LEADER} lost primary status during node4 scaling"
    exit 1
fi
echo "+ Cluster consensus is stable. pgvisor-node4 remains a healthy standby."

echo ""
echo "[10/10] Verifying Web Dashboard dynamically discovered pgvisor-node4..."
DASH_NODES=""
NODE4_FOUND=false
NODE4_VERSION=""
for attempt in 1 2 3 4 5 6 7 8 9 10; do
    DASH_NODES=$(curl -s "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/nodes" 2>/dev/null || true)
    if echo "${DASH_NODES}" | grep -q "pgvisor-node4"; then
        NODE4_FOUND=true
        if command -v jq &> /dev/null; then
            NODE4_VERSION=$(echo "${DASH_NODES}" | jq -r '.[] | select(.address | contains("node4")) | .pg_version' 2>/dev/null || true)
        fi
        break
    fi
    sleep 1
done

if [[ "${NODE4_FOUND}" != "true" ]]; then
    echo "ERROR: Web Dashboard failed to dynamically discover pgvisor-node4. Response: ${DASH_NODES}"
    exit 1
fi
echo "+ Web Dashboard dynamically discovered pgvisor-node4 in cluster topology!"
if [[ -n "${NODE4_VERSION}" ]]; then
    echo "+ Web Dashboard pgvisor-node4 reports PostgreSQL version: ${NODE4_VERSION}"
fi

echo ""
echo "========================================================="
echo "  Scale Out (Add New Node) Test PASSED Successfully!     "
echo "========================================================="
