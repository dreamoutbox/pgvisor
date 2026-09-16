#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Read/Write Routing Assertion Test (Self-Contained Concurrent Profile)
#
# Verifies that the proxy correctly routes SQL by kind:
#   1. Plain SELECT routes to a standby replica (pg_is_in_recovery = t).
#   2. DDL / DML routes to the Raft leader (only the leader accepts writes).
#   3. After writes, subsequent plain SELECTs still route back to a standby.
#   4. A SELECT inside an explicit BEGIN...COMMIT block is pinned to the leader
#      (TransactionTracker keeps the whole transaction on one connection).
#   5. Plain SELECTs distribute across standbys via round-robin (hits node3).
#   6. When a standby (node2) is stopped, all reads route to the remaining standby (node3).
#
# Observable routing signal: pg_is_in_recovery()
#   - Returns 'f' on the leader  (primary, read-write)
#   - Returns 't' on a standby  (replica, read-only)
#
# All output is strictly clean plain text (no ANSI escape codes).
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
# shellcheck source=tests/lib/cluster.sh
source "${SCRIPT_DIR}/lib/cluster.sh"
# shellcheck source=tests/lib/helper.sh
source "${SCRIPT_DIR}/lib/helper.sh"

# Pre-defined test port & project constants
readonly TEST_PROXY_PORT=6532
readonly TEST_DASHBOARD_PORT=9180
readonly TEST_MINIO_PORT=10100
readonly TEST_MINIO_CONSOLE=10101
readonly PROJECT_NAME="pgvisor-routing"
readonly COMPOSE_FILE="${REPO_ROOT}/composes/docker-compose.routing.yml"

PROXY_HOST="${PGVISOR_HOST:-localhost}"
PROXY_PORT="${PGVISOR_PORT:-${TEST_PROXY_PORT}}"
DASHBOARD_URL="${PGVISOR_DASHBOARD_URL:-http://localhost:${TEST_DASHBOARD_PORT}}"

ADMIN_TOKEN="${PGVISOR_ADMIN_TOKEN:-postgres}"
AUTH_HEADER=()
if [ -n "${ADMIN_TOKEN}" ]; then
    AUTH_HEADER=(-H "Authorization: Bearer ${ADMIN_TOKEN}")
fi

NODE1_CONTAINER="${PROJECT_NAME}-node1"
NODE2_CONTAINER="${PROJECT_NAME}-node2"
NODE3_CONTAINER="${PROJECT_NAME}-node3"

if [ ! -f "${COMPOSE_FILE}" ]; then
    echo "Error: Compose file not found at ${COMPOSE_FILE}"
    echo "Run: ./scripts/generate-test-composes.sh"
    exit 1
fi

cleanup() {
    echo "Tearing down cluster ${PROJECT_NAME}..."
    cluster_down "${PROJECT_NAME}" "${COMPOSE_FILE}"
}
trap cleanup EXIT

# Helper: run a query through the proxy with up to N retries (replication-lag tolerance).
run_proxy_sql() {
    local query="$1"
    local max_attempts="${2:-5}"
    local output=""
    local attempt
    for attempt in $(seq 1 "${max_attempts}"); do
        if command -v psql &> /dev/null; then
            if output=$(PGPASSWORD="" psql -h "${PROXY_HOST}" -p "${PROXY_PORT}" -U postgres -d postgres -t -A -c "${query}" 2>/dev/null); then
                echo "${output}"
                return 0
            fi
        else
            if output=$(docker compose -p "${PROJECT_NAME}" -f "${COMPOSE_FILE}" exec -T pgvisor-proxy \
                        psql -h localhost -p 5432 -U postgres -d postgres -t -A -c "${query}" 2>/dev/null); then
                echo "${output}"
                return 0
            fi
        fi
        sleep 1
    done
    echo "${output:-}"
    return 1
}


echo "========================================================="
echo "  PgVisor Read/Write Routing Assertion Test"
echo "  Project: ${PROJECT_NAME} | Proxy port: ${PROXY_PORT}"
echo "========================================================="

echo "[0/8] Starting isolated test cluster ${PROJECT_NAME}..."
cluster_down "${PROJECT_NAME}" "${COMPOSE_FILE}"
cluster_up   "${PROJECT_NAME}" "${COMPOSE_FILE}"

echo "Waiting for cluster containers to report healthy..."
wait_for_healthy 120 "${PROJECT_NAME}-minio" "${NODE1_CONTAINER}" "${NODE2_CONTAINER}" "${NODE3_CONTAINER}"

echo "Waiting for proxy to become ready..."
wait_for_proxy_ready "${DASHBOARD_URL}" 60 "${AUTH_HEADER[@]}"

# ---------------------------------------------------------------
# Pre-flight: confirm cluster baseline (node1=leader, node2/3=standbys)
# ---------------------------------------------------------------
echo "[1/8] Verifying initial cluster topology via direct node queries..."

NODE1_RECOVERY=""
NODE2_RECOVERY=""
NODE3_RECOVERY=""
for attempt in 1 2 3 4 5 6 7 8 9 10; do
    NODE1_RECOVERY=$(run_node_sql "${NODE1_CONTAINER}" "SELECT pg_is_in_recovery();")
    NODE2_RECOVERY=$(run_node_sql "${NODE2_CONTAINER}" "SELECT pg_is_in_recovery();")
    NODE3_RECOVERY=$(run_node_sql "${NODE3_CONTAINER}" "SELECT pg_is_in_recovery();")
    if [ "${NODE1_RECOVERY}" = "f" ] && [ "${NODE2_RECOVERY}" = "t" ] && [ "${NODE3_RECOVERY}" = "t" ]; then
        break
    fi
    sleep 2
done
if [ "${NODE1_RECOVERY}" != "f" ]; then
    echo "FAIL [Pre-flight]: ${NODE1_CONTAINER} expected to be primary (pg_is_in_recovery=f), got: ${NODE1_RECOVERY}"
    exit 1
fi
if [ "${NODE2_RECOVERY}" != "t" ] || [ "${NODE3_RECOVERY}" != "t" ]; then
    echo "FAIL [Pre-flight]: node2 and node3 must be standbys (pg_is_in_recovery=t)"
    echo "  node2: ${NODE2_RECOVERY}, node3: ${NODE3_RECOVERY}"
    exit 1
fi
echo "  CONFIRMED: node1=primary(f), node2=standby(t), node3=standby(t)."

# ---------------------------------------------------------------
# Scenario 1: Plain SELECT routes to a standby (pg_is_in_recovery=t)
# ---------------------------------------------------------------
echo "[2/8] Scenario 1: plain SELECT routes to standby replica..."
RESULT=""
for attempt in 1 2 3 4 5; do
    RESULT=$(run_proxy_sql "SELECT pg_is_in_recovery();" || echo "err")
    if [ "${RESULT}" = "t" ]; then
        break
    fi
    sleep 1
done
if [ "${RESULT}" != "t" ]; then
    echo "FAIL [Scenario 1]: Expected pg_is_in_recovery=t (standby) via proxy SELECT, got: '${RESULT}'"
    exit 1
fi
echo "  SUCCESS: Scenario 1 — plain SELECT routed to standby (pg_is_in_recovery=${RESULT})."

# ---------------------------------------------------------------
# Scenario 2: DDL routes to leader — CREATE TABLE succeeds
# ---------------------------------------------------------------
echo "[3/8] Scenario 2: DDL (CREATE TABLE) routes to leader..."
run_proxy_sql "DROP TABLE IF EXISTS rw_routing_probe;" > /dev/null
CREATE_RESULT=$(run_proxy_sql "CREATE TABLE rw_routing_probe (id SERIAL PRIMARY KEY, val TEXT NOT NULL);" || echo "err")
if [ "${CREATE_RESULT}" = "err" ]; then
    echo "FAIL [Scenario 2]: CREATE TABLE via proxy failed — DDL was not routed to leader."
    exit 1
fi
echo "  SUCCESS: Scenario 2 — CREATE TABLE succeeded (routed to leader)."

# ---------------------------------------------------------------
# Scenario 3: DML (INSERT) routes to leader; subsequent plain SELECT still hits standby
# ---------------------------------------------------------------
echo "[4/8] Scenario 3: INSERT routes to leader; subsequent plain SELECT still routes to standby..."
INSERT_RESULT=$(run_proxy_sql "INSERT INTO rw_routing_probe (val) VALUES ('alpha');" || echo "err")
if [ "${INSERT_RESULT}" = "err" ]; then
    echo "FAIL [Scenario 3]: INSERT via proxy failed — DML was not routed to leader."
    exit 1
fi

# Subsequent plain SELECT must still land on a standby (pg_is_in_recovery=t).
POST_WRITE_READ=""
for attempt in 1 2 3 4 5; do
    POST_WRITE_READ=$(run_proxy_sql "SELECT pg_is_in_recovery();" || echo "err")
    if [ "${POST_WRITE_READ}" = "t" ]; then
        break
    fi
    sleep 1
done
if [ "${POST_WRITE_READ}" != "t" ]; then
    echo "FAIL [Scenario 3]: After INSERT, plain SELECT expected to route to standby (t), got: '${POST_WRITE_READ}'"
    exit 1
fi
echo "  SUCCESS: Scenario 3 — INSERT routed to leader; post-write SELECT still hits standby (${POST_WRITE_READ})."

# ---------------------------------------------------------------
# Scenario 4: SELECT inside explicit BEGIN...COMMIT is pinned to leader (pg_is_in_recovery=f)
#
# Once BEGIN is issued, TransactionTracker marks the session as in-transaction.
# All subsequent statements (even plain SELECTs) must stay on the same leader
# connection until COMMIT/ROLLBACK returns the session to Idle.
#
# Each statement is a separate psql invocation (separate TCP connection) to
# exercise the proxy's per-connection TransactionTracker state. We use a
# single psql session with multiple -c flags to keep the transaction open.
# ---------------------------------------------------------------
echo "[5/8] Scenario 4: SELECT inside BEGIN...COMMIT is pinned to leader..."

IN_TXN_RECOVERY="err"
if command -v psql &> /dev/null; then
    # Single psql session: BEGIN, then SELECT pg_is_in_recovery(), then COMMIT.
    # -c flags execute in sequence on the same connection; the proxy sees one
    # TCP session so the TransactionTracker state is preserved across -c flags.
    IN_TXN_RECOVERY=$(PGPASSWORD="" psql -h "${PROXY_HOST}" -p "${PROXY_PORT}" \
        -U postgres -d postgres \
        --set ON_ERROR_STOP=on -t -A \
        -c "BEGIN;" \
        -c "SELECT pg_is_in_recovery();" \
        -c "COMMIT;" 2>/dev/null | grep -E '^[ft]$' | head -1 || echo "err")
else
    IN_TXN_RECOVERY=$(docker compose -p "${PROJECT_NAME}" -f "${COMPOSE_FILE}" exec -T pgvisor-proxy \
        psql -h localhost -p 5432 -U postgres -d postgres \
        --set ON_ERROR_STOP=on -t -A \
        -c "BEGIN;" \
        -c "SELECT pg_is_in_recovery();" \
        -c "COMMIT;" 2>/dev/null | grep -E '^[ft]$' | head -1 || echo "err")
fi

if [ "${IN_TXN_RECOVERY}" != "f" ]; then
    echo "FAIL [Scenario 4]: SELECT inside transaction expected pg_is_in_recovery=f (leader), got: '${IN_TXN_RECOVERY}'"
    echo "  This means the proxy incorrectly re-routed a SELECT to a standby mid-transaction."
    exit 1
fi
echo "  SUCCESS: Scenario 4 — SELECT inside BEGIN...COMMIT pinned to leader (pg_is_in_recovery=${IN_TXN_RECOVERY})."

# ---------------------------------------------------------------
# Scenario 5: Round-robin sends reads to node3
# ---------------------------------------------------------------
echo "[6/8] Scenario 5: round-robin routes reads to node3..."
NODE2_IP=$(docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' "${NODE2_CONTAINER}" 2>/dev/null || echo "")
NODE3_IP=$(docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' "${NODE3_CONTAINER}" 2>/dev/null || echo "")
if [ -z "${NODE2_IP}" ] || [ -z "${NODE3_IP}" ]; then
    NODE2_IP=$(run_node_sql "${NODE2_CONTAINER}" "SELECT host(inet_server_addr());" || echo "")
    NODE3_IP=$(run_node_sql "${NODE3_CONTAINER}" "SELECT host(inet_server_addr());" || echo "")
fi

if [ -z "${NODE2_IP}" ] || [ -z "${NODE3_IP}" ] || [ "${NODE2_IP}" = "${NODE3_IP}" ]; then
    echo "FAIL [Scenario 5]: Could not determine distinct IP addresses for node2 and node3 (node2='${NODE2_IP}', node3='${NODE3_IP}')"
    exit 1
fi

HIT_NODE3=false
for i in $(seq 1 10); do
    IP=$(run_proxy_sql "SELECT host(inet_server_addr());" || echo "")
    if [ "${IP}" = "${NODE3_IP}" ]; then
        HIT_NODE3=true
        break
    fi
done

if [ "${HIT_NODE3}" != "true" ]; then
    echo "FAIL [Scenario 5]: node3 never received a read query in 10 round-robin attempts"
    exit 1
fi
echo "  SUCCESS: Scenario 5 — node3 (${NODE3_IP}) served at least one read query via round-robin."

# ---------------------------------------------------------------
# Scenario 6: Node 2 down — node 3 is sole read target
# ---------------------------------------------------------------
echo "[7/8] Scenario 6: node2 down, all reads must route to node3..."
stop_node "${NODE2_CONTAINER}"

# Settle budget: proxy topology monitor polls /control/status every 500ms
sleep 5

ALL_TO_NODE3=true
for i in $(seq 1 5); do
    IP=$(run_proxy_sql "SELECT host(inet_server_addr());" || echo "")
    if [ "${IP}" != "${NODE3_IP}" ]; then
        ALL_TO_NODE3=false
        echo "  attempt ${i}: got IP '${IP}', expected node3 '${NODE3_IP}'"
    fi
    sleep 1
done

# Restore node2 before assertion check so cluster is intact regardless
start_node "${NODE2_CONTAINER}" "${PROJECT_NAME}" "${COMPOSE_FILE}" pgvisor-node2

if [ "${ALL_TO_NODE3}" != "true" ]; then
    echo "FAIL [Scenario 6]: reads did not exclusively route to node3 after node2 was stopped"
    exit 1
fi
echo "  SUCCESS: Scenario 6 — with node2 down, all reads routed exclusively to node3 (${NODE3_IP})."

# ---------------------------------------------------------------
# Cleanup
# ---------------------------------------------------------------
echo "[8/8] Cleanup and final dashboard health check..."
run_proxy_sql "DROP TABLE IF EXISTS rw_routing_probe;" > /dev/null

STATUS_CODE=$(curl -s -o /dev/null -w "%{http_code}" "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/status")
if [ "${STATUS_CODE}" != "200" ]; then
    echo "FAIL: Dashboard /api/status returned ${STATUS_CODE}"
    exit 1
fi
echo "  SUCCESS: Dashboard healthy (HTTP ${STATUS_CODE})."

echo ""
echo "========================================================="
echo "  All PgVisor routing assertion tests passed!"
echo "========================================================="
