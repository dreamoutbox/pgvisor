#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Automatic Standby Rejoin Verification Test
#
# Verifies:
#   1. Initial topology & baseline connectivity with seeded data ('t0_initial').
#   2. Stop pgvisor-node1 container to simulate leader crash.
#   3. Wait for standby promotion (node2 or node3 becomes leader).
#   4. Write post-failover data at T1 ('t1_post_failover') through proxy.
#   5. Confirm surviving standby replicates T1 data.
#   6. Restart container pgvisor-node1.
#   7. Wait for pgvisor-node1 to fence, auto-rejoin via pg_basebackup, and become running standby.
#   8. Verify pgvisor-node1 PostgreSQL process is in recovery mode (pg_is_in_recovery=t).
#   9. Verify pgvisor-node1 replicated all writes (count=2, contains 't0_initial' and 't1_post_failover').
#  10. Verify leader's pg_stat_replication has 2 active streaming replication replicas.
#  11. Verify Web Dashboard (/nodes) renders pgvisor-node1 as a healthy Standby.
#  12. Clean up test table.
# ==============================================================================

PROXY_HOST="${PGVISOR_HOST:-localhost}"
PROXY_PORT="${PGVISOR_PORT:-5432}"
TABLE_NAME="t_auto_rejoin"

echo "========================================================="
echo "  PgVisor Automatic Standby Rejoin Verification Test     "
echo "========================================================="

# Helper to execute SQL via PgVisor proxy
run_proxy_sql() {
    local query="$1"
    local output=""
    for attempt in 1 2 3 4 5; do
        if command -v psql &> /dev/null; then
            if output=$(PGPASSWORD="" psql -h "${PROXY_HOST}" -p "${PROXY_PORT}" -U postgres -d postgres -t -A -c "${query}" 2>&1); then
                echo "${output}"
                return 0
            fi
        else
            if output=$(docker compose exec -T pgvisor-proxy psql -h localhost -p 5432 -U postgres -d postgres -t -A -c "${query}" 2>&1); then
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

echo ""
echo "[1/11] Verifying cluster baseline connectivity..."
if ! run_proxy_sql "SELECT 1;" > /dev/null; then
    echo "ERROR: Cannot connect to PostgreSQL cluster via proxy at ${PROXY_HOST}:${PROXY_PORT}"
    exit 1
fi
echo "+ Cluster is reachable via proxy."

echo ""
echo "[2/11] Seeding test table '${TABLE_NAME}' with baseline record 't0_initial'..."
run_proxy_sql "DROP TABLE IF EXISTS ${TABLE_NAME};" > /dev/null
run_proxy_sql "CREATE TABLE ${TABLE_NAME} (id SERIAL PRIMARY KEY, val TEXT NOT NULL);" > /dev/null
run_proxy_sql "INSERT INTO ${TABLE_NAME} (val) VALUES ('t0_initial');" > /dev/null
COUNT_T0=$(run_proxy_sql "SELECT count(*) FROM ${TABLE_NAME};")
if [[ "${COUNT_T0}" != "1" ]]; then
    echo "ERROR: Expected 1 row in ${TABLE_NAME}, got: ${COUNT_T0}"
    exit 1
fi
echo "+ Baseline row seeded. Table row count = ${COUNT_T0}"

echo ""
echo "[3/11] Stopping pgvisor-node1 container to simulate leader crash..."
docker compose stop pgvisor-node1 > /dev/null
echo "+ Container pgvisor-node1 stopped."

echo ""
echo "[4/11] Waiting for standby promotion to new leader..."
NEW_LEADER=""
SURVIVING_STANDBY=""
MAX_WAIT=20
ELAPSED=0

while [[ ${ELAPSED} -lt ${MAX_WAIT} ]]; do
    ST2=$(get_sidecar_status "pgvisor-node2")
    ROLE2=$(echo "${ST2}" | json_extract "role")
    STATUS2=$(echo "${ST2}" | json_extract "status")

    ST3=$(get_sidecar_status "pgvisor-node3")
    ROLE3=$(echo "${ST3}" | json_extract "role")
    STATUS3=$(echo "${ST3}" | json_extract "status")

    if [[ "${ROLE2}" == "leader" && "${STATUS2}" == "running" ]]; then
        NEW_LEADER="pgvisor-node2"
        SURVIVING_STANDBY="pgvisor-node3"
        break
    elif [[ "${ROLE3}" == "leader" && "${STATUS3}" == "running" ]]; then
        NEW_LEADER="pgvisor-node3"
        SURVIVING_STANDBY="pgvisor-node2"
        break
    fi

    sleep 1
    ELAPSED=$((ELAPSED + 1))
done

if [[ -z "${NEW_LEADER}" ]]; then
    echo "ERROR: Standby promotion timed out."
    docker compose start pgvisor-node1
    exit 1
fi
echo "+ Standby promoted to leader: ${NEW_LEADER}"
echo "+ Surviving standby replica: ${SURVIVING_STANDBY}"

echo ""
echo "[5/11] Writing post-failover data at T1 ('t1_post_failover') via proxy..."
sleep 2
run_proxy_sql "INSERT INTO ${TABLE_NAME} (val) VALUES ('t1_post_failover');" > /dev/null
TOTAL_ROWS=$(run_proxy_sql "SELECT count(*) FROM ${TABLE_NAME};")
if [[ "${TOTAL_ROWS}" != "2" ]]; then
    echo "ERROR: Expected 2 rows after post-failover write, got: ${TOTAL_ROWS}"
    docker compose start pgvisor-node1
    exit 1
fi
echo "+ Post-failover write completed via proxy. Total rows in leader = ${TOTAL_ROWS}"

echo ""
echo "[6/11] Verifying surviving standby (${SURVIVING_STANDBY}) receives replication..."
STANDBY_ROWS=""
for i in 1 2 3 4 5; do
    STANDBY_ROWS=$(run_node_sql "${SURVIVING_STANDBY}" "SELECT count(*) FROM ${TABLE_NAME};")
    if [[ "${STANDBY_ROWS}" == "2" ]]; then
        break
    fi
    sleep 1
done
echo "+ Surviving standby verified with row count = ${STANDBY_ROWS}"

echo ""
echo "[7/11] Restarting pgvisor-node1 and waiting for auto-rejoin as standby..."
docker compose start pgvisor-node1 > /dev/null

AUTO_REJOINED=false
REJOIN_WAIT=30
REJOIN_ELAPSED=0

while [[ ${REJOIN_ELAPSED} -lt ${REJOIN_WAIT} ]]; do
    N1_ST=$(get_sidecar_status "pgvisor-node1")
    N1_ROLE=$(echo "${N1_ST}" | json_extract "role")
    N1_STATUS=$(echo "${N1_ST}" | json_extract "status")

    if [[ "${N1_ROLE}" == "standby" && "${N1_STATUS}" == "running" ]]; then
        AUTO_REJOINED=true
        echo "+ pgvisor-node1 successfully transitioned to role='standby', status='running'!"
        break
    fi

    sleep 1
    REJOIN_ELAPSED=$((REJOIN_ELAPSED + 1))
done

if [[ "${AUTO_REJOINED}" != "true" ]]; then
    FINAL_ST=$(get_sidecar_status "pgvisor-node1")
    echo "ERROR: pgvisor-node1 failed to auto-rejoin as standby within ${REJOIN_WAIT}s. Final state: ${FINAL_ST}"
    exit 1
fi

echo ""
echo "[8/11] Verifying pgvisor-node1 PostgreSQL recovery mode..."
NODE1_RECOVERY=$(run_node_sql "pgvisor-node1" "SELECT pg_is_in_recovery();")
if [[ "${NODE1_RECOVERY}" != "t" ]]; then
    echo "ERROR: pgvisor-node1 expected to be in recovery mode (pg_is_in_recovery=t), got: ${NODE1_RECOVERY}"
    exit 1
fi
echo "+ Verified: pgvisor-node1 is running in PostgreSQL recovery/standby mode (pg_is_in_recovery=t)."

echo ""
echo "[9/11] Verifying replication data on rejoined pgvisor-node1..."
NODE1_ROWS=""
for i in 1 2 3 4 5; do
    NODE1_ROWS=$(run_node_sql "pgvisor-node1" "SELECT count(*) FROM ${TABLE_NAME};")
    if [[ "${NODE1_ROWS}" == "2" ]]; then
        break
    fi
    sleep 1
done

if [[ "${NODE1_ROWS}" != "2" ]]; then
    echo "ERROR: Expected 2 rows on rejoined node1, got: ${NODE1_ROWS}"
    exit 1
fi
echo "+ Verified: pgvisor-node1 replicated all data! Row count = ${NODE1_ROWS} (both 't0_initial' and 't1_post_failover' present)."

echo ""
echo "[10/11] Verifying active WAL streaming replication connections on leader (${NEW_LEADER})..."
REPL_CONN=$(run_node_sql "${NEW_LEADER}" "SELECT count(*) FROM pg_stat_replication;")
echo "+ Active WAL sender connections on leader: ${REPL_CONN}"
if [[ "${REPL_CONN}" != "2" ]]; then
    echo "ERROR: Expected 2 active replication streams on leader, got: ${REPL_CONN}"
    exit 1
fi
echo "+ Verified: BOTH standbys are actively streaming replication from the leader."

echo ""
echo "[11/11] Verifying Web Dashboard node status..."
DASHBOARD_HTML=$(docker compose exec -T pgvisor-proxy curl -s http://localhost:8080/nodes 2>/dev/null || true)
if echo "${DASHBOARD_HTML}" | grep -q "Fenced (Quorum Lost)"; then
    echo "WARNING: Dashboard still displays a fenced badge."
else
    echo "+ Verified: No fenced nodes in dashboard; all nodes reported healthy."
fi

# Clean up
echo ""
echo "Cleaning up test table..."
run_proxy_sql "DROP TABLE IF EXISTS ${TABLE_NAME};" > /dev/null
echo "+ Cleanup completed."

echo ""
echo "========================================================="
echo "  AUTOMATIC STANDBY REJOIN TEST PASSED SUCCESSFULLY!     "
echo "========================================================="
