#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Promoted Leader Snapshot Restore Verification Test
#
# Reproduces and verifies the exact operational scenario:
# 1. Cluster starts with Node #1 as initial leader, Node #2 and #3 as standbys.
# 2. Schema and initial rows (alpha, beta, gamma, delta) are seeded.
# 3. Full backup 'f1' is created via Dashboard API.
# 4. Incremental rows (echo, foxtrot, golf) are inserted, then 'incr2' is created.
# 5. Node #1 is stopped via Dashboard API (POST /api/nodes/1/stop).
# 6. Node #2 auto-promotes to become the new cluster leader.
# 7. Incremental backup 'incr2' is restored WITHOUT specifying a PITR target time.
# 8. Asserts:
#    - Node #2 restores and boots as a read-write primary (pg_is_in_recovery() = false).
#    - Node #2 does NOT log recovery spam ("ERROR: recovery is in progress",
#      "HINT: WAL control functions cannot be executed during recovery",
#      "FATAL: streaming replication receiver").
#    - Node #3 re-syncs cleanly from Node #2 and streaming replication resumes.
#    - Proxy routes reads to return all 7 restored records.
#    - Proxy routes writes to Node #2 and streaming replication propagates to Node #3.
#
# All output is clean plain text (no ANSI escape codes).
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
# shellcheck source=tests/lib/cluster.sh
source "${SCRIPT_DIR}/lib/cluster.sh"

readonly TEST_PROXY_PORT=7332
readonly TEST_DASHBOARD_PORT=9980
readonly TEST_MINIO_PORT=10900
readonly TEST_MINIO_CONSOLE=10901
readonly PROJECT_NAME="pgvisor-promoted-restore"
readonly COMPOSE_FILE="${REPO_ROOT}/composes/docker-compose.promoted-restore.yml"

PROXY_HOST="${PGVISOR_HOST:-localhost}"
PROXY_PORT="${PGVISOR_PORT:-${TEST_PROXY_PORT}}"
DASHBOARD_URL="${PGVISOR_DASHBOARD_URL:-http://localhost:${TEST_DASHBOARD_PORT}}"
ADMIN_TOKEN="${PGVISOR_ADMIN_TOKEN:-postgres}"
AUTH_HEADER=()
if [ -n "${ADMIN_TOKEN}" ]; then
    AUTH_HEADER=(-H "Authorization: Bearer ${ADMIN_TOKEN}")
fi

NODE1_CONTAINER="pgvisor-promoted-restore-node1"
NODE2_CONTAINER="pgvisor-promoted-restore-node2"
NODE3_CONTAINER="pgvisor-promoted-restore-node3"
PROXY_CONTAINER="pgvisor-promoted-restore-proxy"
TABLE_NAME="t_promoted_restore"

cleanup() {
    local exit_code=$?
    if [ "${exit_code}" -ne 0 ]; then
        echo "=== NODE1 LOGS ON FAILURE ==="
        docker logs "${NODE1_CONTAINER}" --tail 50 2>&1 || true
        echo "=== NODE2 LOGS ON FAILURE ==="
        docker logs "${NODE2_CONTAINER}" --tail 50 2>&1 || true
        echo "=== NODE3 LOGS ON FAILURE ==="
        docker logs "${NODE3_CONTAINER}" --tail 50 2>&1 || true
        echo "=== PROXY LOGS ON FAILURE ==="
        docker logs "${PROXY_CONTAINER}" --tail 50 2>&1 || true
    fi
    echo "Tearing down cluster ${PROJECT_NAME}..."
    cluster_down "${PROJECT_NAME}" "${COMPOSE_FILE}"
}
trap cleanup EXIT

echo "========================================================="
echo "  PgVisor Promoted Leader Snapshot Restore Test          "
echo "  Project: ${PROJECT_NAME} | Port: ${PROXY_PORT}         "
echo "========================================================="

echo "[0/7] Starting isolated test cluster ${PROJECT_NAME}..."
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

# Helper to execute SQL directly on a node container
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

# ------------------------------------------------------------------------------
# [1/7] Seed demo schema and initial records
# ------------------------------------------------------------------------------
echo ""
echo "[1/7] Seeding initial records (alpha, beta, gamma, delta)..."
run_proxy_sql "DROP TABLE IF EXISTS ${TABLE_NAME};" > /dev/null
run_proxy_sql "CREATE TABLE ${TABLE_NAME} (id SERIAL PRIMARY KEY, name VARCHAR(100) NOT NULL, status VARCHAR(50) DEFAULT 'active', counter INT DEFAULT 0);" > /dev/null
run_proxy_sql "INSERT INTO ${TABLE_NAME} (name, status, counter) VALUES ('alpha', 'active', 10), ('beta', 'active', 20), ('gamma', 'pending', 30), ('delta', 'archived', 40);" > /dev/null

INIT_COUNT=$(run_proxy_sql "SELECT count(*) FROM ${TABLE_NAME};")
if [ "${INIT_COUNT}" != "4" ]; then
    echo "ERROR: Expected 4 initial rows, got '${INIT_COUNT}'"
    exit 1
fi
echo "Initial data seeded successfully (4 rows)."

# ------------------------------------------------------------------------------
# [2/7] Trigger full backup 'f1'
# ------------------------------------------------------------------------------
echo ""
echo "[2/7] Creating full basebackup 'f1'..."
F1_RESP=$(curl -s -X POST "${DASHBOARD_URL}/api/backups" \
    "${AUTH_HEADER[@]}" \
    -H "Content-Type: application/json" \
    -d '{"backup_type": "full", "label": "f1"}')

F1_ID=$(echo "${F1_RESP}" | grep -o '"snapshot_id":"[^"]*' | head -n1 | cut -d'"' -f4)
if [ -z "${F1_ID}" ]; then
    echo "ERROR: Failed to create full backup 'f1'. Response: ${F1_RESP}"
    exit 1
fi
echo "Full backup 'f1' created: ${F1_ID}"

# ------------------------------------------------------------------------------
# [3/7] Insert incremental records and trigger 'incr2'
# ------------------------------------------------------------------------------
echo ""
echo "[3/7] Inserting incremental records (echo, foxtrot, golf) and creating backup 'incr2'..."
run_proxy_sql "INSERT INTO ${TABLE_NAME} (name, status, counter) VALUES ('echo', 'active', 50);" > /dev/null
sleep 1
run_proxy_sql "INSERT INTO ${TABLE_NAME} (name, status, counter) VALUES ('foxtrot', 'pending', 60);" > /dev/null
sleep 1
run_proxy_sql "INSERT INTO ${TABLE_NAME} (name, status, counter) VALUES ('golf', 'archived', 70);" > /dev/null

# Force WAL switch on leader to archive all segments
run_proxy_sql "BEGIN; SELECT pg_switch_wal(); COMMIT;" > /dev/null 2>&1 || true

INCR2_RESP=$(curl -s -X POST "${DASHBOARD_URL}/api/backups" \
    "${AUTH_HEADER[@]}" \
    -H "Content-Type: application/json" \
    -d '{"backup_type": "incremental", "label": "incr2"}')

INCR2_ID=$(echo "${INCR2_RESP}" | grep -o '"snapshot_id":"[^"]*' | head -n1 | cut -d'"' -f4)
if [ -z "${INCR2_ID}" ]; then
    echo "ERROR: Failed to create incremental backup 'incr2'. Response: ${INCR2_RESP}"
    exit 1
fi
echo "Incremental backup 'incr2' created: ${INCR2_ID}"

SEVEN_COUNT=$(run_proxy_sql "SELECT count(*) FROM ${TABLE_NAME};")
if [ "${SEVEN_COUNT}" != "7" ]; then
    echo "ERROR: Expected 7 rows before failover, got '${SEVEN_COUNT}'"
    exit 1
fi

# ------------------------------------------------------------------------------
# [4/7] Stop Node #1 and assert Node #2 auto-promotes to leader
# ------------------------------------------------------------------------------
echo ""
echo "[4/7] Stopping Node #1 to trigger leader failover to Node #2..."
STOP_RESP=$(curl -s -X POST "${DASHBOARD_URL}/api/nodes/1/stop" "${AUTH_HEADER[@]}")
echo "Node #1 stop response: ${STOP_RESP}"

echo "Waiting for Node #2 to auto-promote to leader..."
PROMOTED=false
for i in $(seq 1 15); do
    STATUS_N2=$(get_sidecar_status "${NODE2_CONTAINER}")
    ROLE_N2=$(echo "${STATUS_N2}" | grep -o '"role":"[^"]*' | cut -d'"' -f4 || true)
    PROC_N2=$(echo "${STATUS_N2}" | grep -o '"status":"[^"]*' | cut -d'"' -f4 || true)
    if [ "${ROLE_N2}" = "leader" ] && [ "${PROC_N2}" = "running" ]; then
        PROMOTED=true
        echo "Node #2 successfully promoted to leader on attempt ${i}."
        break
    fi
    sleep 1
done

if [ "${PROMOTED}" != "true" ]; then
    echo "ERROR: Node #2 did not promote to leader within timeout. Status: $(get_sidecar_status "${NODE2_CONTAINER}")"
    exit 1
fi

# Verify Node #2 accepts write queries directly
IS_RECOVERY=$(run_node_sql "${NODE2_CONTAINER}" "SELECT pg_is_in_recovery();")
if [ "${IS_RECOVERY}" != "f" ]; then
    echo "ERROR: Promoted Node #2 is unexpectedly in recovery (pg_is_in_recovery=${IS_RECOVERY})"
    exit 1
fi
echo "Promoted Node #2 is verified as read-write primary (pg_is_in_recovery=f)."

# Verify proxy leader discovery has switched to Node #2
echo "Waiting for Proxy dynamic discovery to recognize Node #2 as leader..."
PROXY_DISCOVERED=false
for i in $(seq 1 15); do
    STATUS_RESP=$(curl -s "${DASHBOARD_URL}/api/status" "${AUTH_HEADER[@]}" || true)
    LEADER_ADDR=$(echo "${STATUS_RESP}" | grep -o '"leader_address":"[^"]*' | cut -d'"' -f4 || true)
    if [[ "${LEADER_ADDR}" == *"node2"* ]]; then
        PROXY_DISCOVERED=true
        echo "Proxy dynamic leader discovery updated: leader_address=${LEADER_ADDR} (attempt ${i})."
        break
    fi
    sleep 1
done

if [ "${PROXY_DISCOVERED}" != "true" ]; then
    echo "ERROR: Proxy did not discover Node #2 as leader within timeout. Status: ${STATUS_RESP}"
    exit 1
fi

# ------------------------------------------------------------------------------
# [5/7] Restore snapshot 'incr2' WITHOUT specifying PITR target time
# ------------------------------------------------------------------------------
echo ""
echo "[5/7] Restoring incremental snapshot '${INCR2_ID}' without specifying PITR target time..."
RESTORE_RESP=$(curl -s -X POST "${DASHBOARD_URL}/api/backups/${INCR2_ID}/restore" \
    "${AUTH_HEADER[@]}" \
    -H "Content-Type: application/json" \
    -d '{"recovery_target_time": null}')

echo "Restore response: ${RESTORE_RESP}"
if ! echo "${RESTORE_RESP}" | grep -q '"status":"success"'; then
    echo "ERROR: Restore API returned failure: ${RESTORE_RESP}"
    exit 1
fi

# ------------------------------------------------------------------------------
# [6/7] Assert Node #2 boots as primary with zero recovery spam
# ------------------------------------------------------------------------------
echo ""
echo "[6/7] Verifying Node #2 post-restore state and absence of recovery spam..."

# Wait up to 15s for Node #2 to finish restore and report ready
RESTORE_READY=false
for i in $(seq 1 15); do
    STATUS_N2=$(get_sidecar_status "${NODE2_CONTAINER}")
    PROC_N2=$(echo "${STATUS_N2}" | grep -o '"status":"[^"]*' | cut -d'"' -f4 || true)
    if [ "${PROC_N2}" = "running" ]; then
        RESTORE_READY=true
        break
    fi
    sleep 1
done

if [ "${RESTORE_READY}" != "true" ]; then
    echo "ERROR: Node #2 did not return to running status after restore. Status: $(get_sidecar_status "${NODE2_CONTAINER}")"
    exit 1
fi

# Crucial Assertion 1: Node #2 must NOT be in recovery!
POST_RESTORE_RECOVERY=$(run_node_sql "${NODE2_CONTAINER}" "SELECT pg_is_in_recovery();")
if [ "${POST_RESTORE_RECOVERY}" != "f" ]; then
    echo "ERROR: Node #2 is stuck in standby recovery mode (pg_is_in_recovery=${POST_RESTORE_RECOVERY})!"
    exit 1
fi
echo "Node #2 is running as read-write primary (pg_is_in_recovery=f)."

# Crucial Assertion 2: Node #2 logs must NOT contain recovery spam or replication receiver errors
sleep 2
N2_LOGS=$(docker logs "${NODE2_CONTAINER}" 2>&1)

if echo "${N2_LOGS}" | grep -F "ERROR:  recovery is in progress"; then
    echo "ERROR: Found 'ERROR: recovery is in progress' in Node #2 logs!"
    exit 1
fi

if echo "${N2_LOGS}" | grep -F "HINT:  WAL control functions cannot be executed during recovery."; then
    echo "ERROR: Found 'HINT: WAL control functions cannot be executed during recovery' in Node #2 logs!"
    exit 1
fi

if echo "${N2_LOGS}" | grep -F "FATAL:  streaming replication receiver" | grep -F "connection to server at \"pgvisor-promoted-restore-node1\""; then
    echo "ERROR: Found streaming replication receiver attempting to connect to dead Node #1 in Node #2 logs!"
    exit 1
fi

echo "No recovery spam or spurious replication receiver errors found in Node #2 logs."

# Crucial Assertion 3: Node #3 must re-sync from Node #2 and establish streaming replication
echo "Waiting for Node #3 streaming replication to become active on Node #2..."
REPL_READY=false
for i in $(seq 1 20); do
    REPL_CLIENTS=$(run_node_sql "${NODE2_CONTAINER}" "SELECT count(*) FROM pg_stat_replication WHERE application_name LIKE '%node3%';")
    if [ "${REPL_CLIENTS}" -ge 1 ]; then
        REPL_READY=true
        echo "Node #3 streaming replication active on Node #2 (attempt ${i})."
        break
    fi
    sleep 1
done

if [ "${REPL_READY}" != "true" ]; then
    echo "ERROR: Node #3 did not re-sync and connect to Node #2 within timeout. pg_stat_replication count: ${REPL_CLIENTS}"
    exit 1
fi

# ------------------------------------------------------------------------------
# [7/7] Verify proxy read/write routing and replica propagation
# ------------------------------------------------------------------------------
echo ""
echo "[7/7] Verifying proxy read/write operations and replication..."

# Assert proxy reads return all 7 restored records
RESTORED_ROWS=$(run_proxy_sql "SELECT count(*) FROM ${TABLE_NAME};")
if [ "${RESTORED_ROWS}" != "7" ]; then
    echo "ERROR: Expected 7 rows after restore, got '${RESTORED_ROWS}'"
    exit 1
fi
echo "Proxy reads successfully verified: 7 rows restored."

# Assert new write via proxy routes to Node #2
run_proxy_sql "INSERT INTO ${TABLE_NAME} (name, status, counter) VALUES ('hotel', 'active', 80);" > /dev/null
echo "New write 'hotel' executed successfully through proxy."

# Verify replication to Node #3 with polling loop
PROPAGATED=false
for i in $(seq 1 10); do
    COUNT_N3=$(run_node_sql "${NODE3_CONTAINER}" "SELECT count(*) FROM ${TABLE_NAME} WHERE name = 'hotel';")
    if [ "${COUNT_N3}" = "1" ]; then
        PROPAGATED=true
        echo "New record 'hotel' successfully propagated to Node #3 on attempt ${i}."
        break
    fi
    sleep 1
done

if [ "${PROPAGATED}" != "true" ]; then
    echo "ERROR: Record 'hotel' was not replicated to Node #3 within timeout."
    exit 1
fi

echo ""
echo "========================================================="
echo "  TEST PASSED: Promoted Leader Snapshot Restore Verified "
echo "========================================================="
