#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Timeline Divergence, Recovery Target Overrun, and Multi-Restore Test
#
# Regression test for the multi-timeline divergence and recovery target overrun
# incident documented in knowledges/post-mortem-timeline-divergence-and-recovery-overrun.md:
#
# 1. Seeds table with baseline data (4 rows) and takes full backup 'f1'.
# 2. Slowly inserts incremental rows ('echo', 'foxtrot', 'golf') and triggers 'incr2'.
# 3. Step 1: Restores 'f1' with PITR target stopping after 'foxtrot' (forks to Timeline 2).
# 4. Step 2: Restores 'incr2' on Timeline 1 without target time.
#    -> Asserts standbys do NOT crash with "FATAL: requested timeline 2 is not a child...".
# 5. Step 3: Re-restores 'f1' with PITR target stopping after 'foxtrot'.
#    -> Asserts leader does NOT crash with "FATAL: recovery ended before configured target...".
#    -> Asserts proxy does NOT self-restore to 127.0.0.1:8080 or follow 303 redirects.
#    -> Asserts /tables returns HTTP 200 fast without 30s connection pool timeout hang.
#
# All output is strictly clean plain text (no ANSI escape codes).
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
# shellcheck source=tests/lib/cluster.sh
source "${SCRIPT_DIR}/lib/cluster.sh"

# Test port & project constants
readonly TEST_PROXY_PORT=7032
readonly TEST_DASHBOARD_PORT=9680
readonly TEST_MINIO_PORT=10600
readonly TEST_MINIO_CONSOLE=10601
readonly PROJECT_NAME="pgvisor-timeline-divergence"
readonly COMPOSE_FILE="${REPO_ROOT}/composes/docker-compose.timeline-divergence.yml"

PROXY_HOST="${PGVISOR_HOST:-localhost}"
PROXY_PORT="${PGVISOR_PORT:-${TEST_PROXY_PORT}}"
DASHBOARD_URL="${PGVISOR_DASHBOARD_URL:-http://localhost:${TEST_DASHBOARD_PORT}}"
ADMIN_TOKEN="${PGVISOR_ADMIN_TOKEN:-postgres}"
AUTH_HEADER=()
if [ -n "${ADMIN_TOKEN}" ]; then
    AUTH_HEADER=(-H "Authorization: Bearer ${ADMIN_TOKEN}")
fi
NODE_CONTAINER="${PGVISOR_NODE_CONTAINER:-pgvisor-timeline-divergence-node1}"

echo "========================================================="
echo "  PgVisor Timeline Divergence & Multi-Restore Test"
echo "  Project: ${PROJECT_NAME} | Port: ${PROXY_PORT}"
echo "========================================================="

TEST_SUCCESS=0
cleanup() {
    if [ "${TEST_SUCCESS:-0}" -ne 1 ]; then
        echo "=== Dump on Failure: Node 1 ==="
        docker logs --tail 80 "${PROJECT_NAME}-node1" 2>&1 || true
        echo "=== Dump on Failure: Node 2 ==="
        docker logs --tail 80 "${PROJECT_NAME}-node2" 2>&1 || true
        echo "=== Dump on Failure: Node 3 ==="
        docker logs --tail 80 "${PROJECT_NAME}-node3" 2>&1 || true
        echo "=== Dump on Failure: Proxy ==="
        docker logs --tail 80 "${PROJECT_NAME}-proxy" 2>&1 || true
    fi
    echo "Tearing down cluster ${PROJECT_NAME}..."
    cluster_down "${PROJECT_NAME}" "${COMPOSE_FILE}"
}
trap cleanup EXIT

echo "[0/6] Starting isolated test cluster ${PROJECT_NAME}..."
cluster_down "${PROJECT_NAME}" "${COMPOSE_FILE}"
cluster_up   "${PROJECT_NAME}" "${COMPOSE_FILE}"

echo "Waiting for cluster containers to report healthy..."
wait_for_healthy 120 \
    "${PROJECT_NAME}-minio" \
    "${PROJECT_NAME}-node1" \
    "${PROJECT_NAME}-node2" \
    "${PROJECT_NAME}-node3"
wait_for_proxy_ready "${DASHBOARD_URL}" 60 "${AUTH_HEADER[@]}"

# Helper for executing SQL via psql with automatic reconnection retry
run_sql() {
    local query="$1"
    local output=""
    for attempt in 1 2 3 4 5; do
        if command -v psql &> /dev/null; then
            if output=$(PGPASSWORD="" psql -h "${PROXY_HOST}" -p "${PROXY_PORT}" -U postgres -d postgres -t -A -c "${query}" 2>&1); then
                echo "${output}"
                return 0
            fi
        fi

        # Fallback to direct container execution if local psql fails
        if docker ps --format '{{.Names}}' 2>/dev/null | grep -q "^${NODE_CONTAINER}$"; then
            if output=$(docker exec -i "${NODE_CONTAINER}" psql -U postgres -d postgres -t -A -c "${query}" 2>&1); then
                echo "${output}"
                return 0
            fi
        fi
        sleep 1
    done
    echo "${output}"
    return 1
}

# Helper to extract JSON field using jq or python3
json_extract() {
    local field="$1"
    if command -v jq &> /dev/null; then
        jq -r ".${field} // empty"
    elif command -v python3 &> /dev/null; then
        python3 -c "import sys, json; data = json.load(sys.stdin); print(data.get('${field}', ''))" 2>/dev/null || true
    else
        grep -o "\"${field}\":\"[^\"]*\"" | head -n 1 | cut -d':' -f2 | tr -d '"'
    fi
}

# Helper to trigger backup snapshot via Dashboard API
trigger_backup() {
    local b_type="$1"
    local b_label="$2"
    local resp
    resp=$(curl -s -f -X POST "${DASHBOARD_URL}/api/backups" \
        "${AUTH_HEADER[@]}" \
        -H "Content-Type: application/json" \
        -d "{\"backup_type\": \"${b_type}\", \"label\": \"${b_label}\"}")
    echo "${resp}"
}

# Helper to restore snapshot with optional recovery target time
restore_snapshot() {
    local snapshot_id="$1"
    local target_time="${2:-}"
    local payload="{}"
    if [ -n "${target_time}" ]; then
        payload="{\"recovery_target_time\": \"${target_time}\"}"
    fi

    echo "  Invoking restore API for snapshot ${snapshot_id} (target: '${target_time}')..."
    local resp
    resp=$(curl -s -f -X POST "${DASHBOARD_URL}/api/backups/${snapshot_id}/restore" \
        "${AUTH_HEADER[@]}" \
        -H "Content-Type: application/json" \
        -d "${payload}")
    echo "  Restore API response: ${resp}"
}

# Helper to poll and assert healthy nodes count from /api/status
assert_healthy_nodes() {
    local expected="$1"
    local timeout_secs="${2:-30}"
    local count=""
    local deadline=$(( $(date +%s) + timeout_secs ))

    while [ "$(date +%s)" -lt "${deadline}" ]; do
        local resp
        resp=$(curl -s "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/status" 2>/dev/null || true)
        count=$(echo "${resp}" | json_extract "healthy_nodes")
        if [ "${count}" = "${expected}" ]; then
            echo "✓ Healthy nodes verified: ${count}/${expected}"
            return 0
        fi
        sleep 1
    done

    echo "FAIL: Expected ${expected} healthy nodes within ${timeout_secs}s, got '${count}'" >&2
    echo "--- Cluster Status API Dump ---" >&2
    curl -s "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/status" >&2 || true
    echo "" >&2
    return 1
}

# Helper to poll and assert row count in pgvisor_demo table
assert_row_count() {
    local expected="$1"
    local timeout_secs="${2:-30}"
    local count=""
    local deadline=$(( $(date +%s) + timeout_secs ))

    while [ "$(date +%s)" -lt "${deadline}" ]; do
        count=$(run_sql "SELECT COUNT(*) FROM pgvisor_demo;" 2>/dev/null || true)
        if [ "${count}" = "${expected}" ]; then
            echo "✓ Row count verified: ${count} rows (expected: ${expected})"
            return 0
        fi
        sleep 1
    done

    echo "FAIL: Expected ${expected} rows in pgvisor_demo within ${timeout_secs}s, got '${count}'" >&2
    return 1
}

# Helper to poll and assert golf row count in pgvisor_demo table
assert_golf_count() {
    local expected="$1"
    local timeout_secs="${2:-15}"
    local count=""
    local deadline=$(( $(date +%s) + timeout_secs ))

    while [ "$(date +%s)" -lt "${deadline}" ]; do
        count=$(run_sql "SELECT COUNT(*) FROM pgvisor_demo WHERE name = 'golf';" 2>/dev/null || true)
        if [ "${count}" = "${expected}" ]; then
            return 0
        fi
        sleep 1
    done

    echo "FAIL: Expected ${expected} 'golf' rows in pgvisor_demo within ${timeout_secs}s, got '${count}'" >&2
    return 1
}

# Helper to assert that /tables endpoint responds fast without hanging
assert_tables_fast() {
    local max_secs="${1:-5}"
    local t0 t1 elapsed http_code

    t0=$(date +%s)
    http_code=$(curl -s -o /dev/null -w "%{http_code}" -m "${max_secs}" "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/tables" 2>/dev/null || echo "timeout")
    t1=$(date +%s)
    elapsed=$((t1 - t0))

    if [ "${http_code}" != "200" ]; then
        echo "FAIL: GET /tables returned HTTP ${http_code} after ${elapsed}s (expected 200 within ${max_secs}s)" >&2
        return 1
    fi
    echo "✓ Dashboard /tables responded HTTP 200 in ${elapsed}s (within ${max_secs}s limit)."
    return 0
}

# Helper to switch WAL on leader to trigger archive_command
switch_wal() {
    run_sql "BEGIN; SELECT pg_switch_wal(); COMMIT;" > /dev/null 2>&1 || \
        docker exec -i "${NODE_CONTAINER}" psql -U postgres -d postgres -t -A -c "SELECT pg_switch_wal();" > /dev/null 2>&1 || true
}

# ------------------------------------------------------------------------------
# [1/6] Verify cluster connectivity
# ------------------------------------------------------------------------------
echo ""
echo "[1/6] Verifying initial cluster connectivity..."
if ! run_sql "SELECT 1;" &> /dev/null; then
    echo "FAIL: Cannot connect to PostgreSQL cluster at ${PROXY_HOST}:${PROXY_PORT}" >&2
    exit 1
fi
echo "✓ PostgreSQL cluster proxy is reachable."

# ------------------------------------------------------------------------------
# [2/6] Seed baseline demo data and take full backup 'f1'
# ------------------------------------------------------------------------------
echo ""
echo "[2/6] Seeding baseline demo data (4 rows) and taking full backup 'f1'..."
run_sql "
DROP TABLE IF EXISTS pgvisor_demo;
CREATE TABLE pgvisor_demo (
    id SERIAL PRIMARY KEY,
    name VARCHAR(100) NOT NULL,
    status VARCHAR(50) DEFAULT 'active',
    counter INT DEFAULT 0,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);
INSERT INTO pgvisor_demo (name, status, counter) VALUES
    ('alpha', 'active', 10),
    ('beta', 'active', 20),
    ('gamma', 'pending', 30),
    ('delta', 'archived', 40);
"
switch_wal
sleep 1

RESP_F1=$(trigger_backup "full" "f1")
SNAP_F1=$(echo "${RESP_F1}" | json_extract "snapshot_id")
if [ -z "${SNAP_F1}" ]; then
    echo "FAIL: Failed to create full backup 'f1'. Response: ${RESP_F1}" >&2
    exit 1
fi
echo "✓ Baseline backup 'f1' created successfully: snapshot_id='${SNAP_F1}'"

# ------------------------------------------------------------------------------
# [3/6] Delayed inserts, WAL archive, and incremental backup 'incr2'
# ------------------------------------------------------------------------------
echo ""
echo "[3/6] Performing delayed incremental inserts and taking backup 'incr2'..."

echo "  Inserting 'echo' (row 5)..."
run_sql "INSERT INTO pgvisor_demo (name, status, counter) VALUES ('echo', 'active', 50);"
sleep 1

echo "  Inserting 'foxtrot' (row 6)..."
run_sql "INSERT INTO pgvisor_demo (name, status, counter) VALUES ('foxtrot', 'pending', 60);"

# Compute recovery target timestamp: 1 second after 'foxtrot' was committed
TARGET_TIME=$(run_sql "SELECT to_char(created_at + interval '1 second', 'YYYY-MM-DD HH24:MI:SS') FROM pgvisor_demo WHERE name = 'foxtrot';")
echo "  Computed PITR recovery target timestamp: '${TARGET_TIME}'"

# Sleep 3 seconds so 'golf' commit timestamp is strictly after TARGET_TIME
echo "  Sleeping 3s before inserting 'golf'..."
sleep 3

echo "  Inserting 'golf' (row 7)..."
run_sql "INSERT INTO pgvisor_demo (name, status, counter) VALUES ('golf', 'archived', 70);"

# Switch WAL to flush segments to MinIO archive
echo "  Archiving WAL segments via switch_wal..."
switch_wal
sleep 1

# Trigger incremental backup 'incr2' (contains all 7 rows on Timeline 1)
RESP_INCR2=$(trigger_backup "incremental" "incr2")
SNAP_INCR2=$(echo "${RESP_INCR2}" | json_extract "snapshot_id")
if [ -z "${SNAP_INCR2}" ]; then
    echo "FAIL: Failed to create incremental backup 'incr2'. Response: ${RESP_INCR2}" >&2
    exit 1
fi
echo "✓ Incremental backup 'incr2' created successfully: snapshot_id='${SNAP_INCR2}'"

# Verify initial 7 rows
assert_row_count 7 15

# ------------------------------------------------------------------------------
# [4/6] Step 1: PITR restore from 'f1' stopping after 'foxtrot'
# ------------------------------------------------------------------------------
echo ""
echo "[4/6] Step 1: Restoring 'f1' with recovery_target_time='${TARGET_TIME}'..."
restore_snapshot "${SNAP_F1}" "${TARGET_TIME}"

echo "  Waiting for cluster nodes and proxy to become ready..."
wait_for_healthy 60 \
    "${PROJECT_NAME}-node1" \
    "${PROJECT_NAME}-node2" \
    "${PROJECT_NAME}-node3"
wait_for_proxy_ready "${DASHBOARD_URL}" 30 "${AUTH_HEADER[@]}"
sleep 3

# Verify cluster healthy nodes count is 3
assert_healthy_nodes 3 30

# Verify row count is 6 (alpha..foxtrot present, golf omitted)
assert_row_count 6 30
assert_golf_count 0 15
echo "✓ Step 1 verified: exactly 6 rows present, 'golf' excluded, cluster forked to Timeline 2."

# ------------------------------------------------------------------------------
# [5/6] Step 2: Restore 'incr2' without target time (asserting standbys survive)
# ------------------------------------------------------------------------------
echo ""
echo "[5/6] Step 2: Restoring 'incr2' on Timeline 1 without target timestamp..."
restore_snapshot "${SNAP_INCR2}" ""

echo "  Waiting for cluster nodes and proxy to become ready..."
wait_for_healthy 60 \
    "${PROJECT_NAME}-node1" \
    "${PROJECT_NAME}-node2" \
    "${PROJECT_NAME}-node3"
wait_for_proxy_ready "${DASHBOARD_URL}" 30 "${AUTH_HEADER[@]}"
sleep 3

# Crucial check: Replicas must NOT crash with "requested timeline 2 is not a child of this server's history"
assert_healthy_nodes 3 30

# Verify row count is 7 ('golf' is present on Timeline 1)
assert_row_count 7 30
assert_golf_count 1 15
echo "✓ Step 2 verified: all 7 rows restored, standbys synchronized cleanly on Timeline 1."

# ------------------------------------------------------------------------------
# [6/6] Step 3: Re-restore 'f1' with PITR target (asserting no recovery overrun)
# ------------------------------------------------------------------------------
echo ""
echo "[6/6] Step 3: Re-restoring 'f1' with recovery_target_time='${TARGET_TIME}'..."
restore_snapshot "${SNAP_F1}" "${TARGET_TIME}"

echo "  Waiting for cluster nodes and proxy to become ready..."
wait_for_healthy 60 \
    "${PROJECT_NAME}-node1" \
    "${PROJECT_NAME}-node2" \
    "${PROJECT_NAME}-node3"
wait_for_proxy_ready "${DASHBOARD_URL}" 30 "${AUTH_HEADER[@]}"
sleep 3

# Crucial checks:
# 1. Leader must NOT crash with "recovery ended before configured recovery target was reached"
# 2. Proxy must NOT self-restore or report degraded nodes
assert_healthy_nodes 3 30

# 3. Row count must be exactly 6 ('golf' excluded)
assert_row_count 6 30
assert_golf_count 0 15

# 4. Web Dashboard /tables must load immediately and not hang for 30 seconds
assert_tables_fast 5

echo "✓ Step 3 verified: repeated PITR restore succeeded, 0 crashes, /tables loads fast."

TEST_SUCCESS=1
echo ""
echo "========================================================="
echo "  Timeline Divergence & Multi-Restore Test PASSED!       "
echo "========================================================="
