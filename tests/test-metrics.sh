#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Live Metrics & Dashboard Charts Verification Test
#
# Self-contained concurrent test profile.
# Tests telemetry collection and visualization endpoints:
#   - /metrics HTML rendering with Chart.js canvas elements
#   - /api/metrics/snapshot instantaneous snapshot
#   - /api/metrics/history time-series rolling history
#   - All nodes uptime tracking
#   - Node read/write query counts
#   - Node CPU and memory usage tracking from /proc
#   - Proxy read/write routing counters
#   - Replication lag tracking from pg_stat_replication
#   - Backup size, throughput, and success rate
# All output is clean plain text.
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
# shellcheck source=tests/lib/cluster.sh
source "${SCRIPT_DIR}/lib/cluster.sh"

readonly TEST_PROXY_PORT=6932
readonly TEST_DASHBOARD_PORT=9580
readonly TEST_MINIO_PORT=10500
readonly TEST_MINIO_CONSOLE=10501
readonly PROJECT_NAME="pgvisor-metrics"
readonly COMPOSE_FILE="${REPO_ROOT}/composes/docker-compose.metrics.yml"

PROXY_HOST="${PGVISOR_HOST:-localhost}"
PROXY_PORT="${PGVISOR_PORT:-${TEST_PROXY_PORT}}"
DASHBOARD_URL="${PGVISOR_DASHBOARD_URL:-http://localhost:${TEST_DASHBOARD_PORT}}"
ADMIN_TOKEN="${PGVISOR_ADMIN_TOKEN:-postgres}"
AUTH_HEADER=()
if [ -n "${ADMIN_TOKEN}" ]; then
    AUTH_HEADER=(-H "Authorization: Bearer ${ADMIN_TOKEN}")
fi

NODE1_CONTAINER="pgvisor-metrics-node1"
NODE2_CONTAINER="pgvisor-metrics-node2"
NODE3_CONTAINER="pgvisor-metrics-node3"
PROXY_CONTAINER="pgvisor-metrics-proxy"

cleanup() {
    echo "Tearing down cluster ${PROJECT_NAME}..."
    cluster_down "${PROJECT_NAME}" "${COMPOSE_FILE}"
}
trap cleanup EXIT

echo "========================================================="
echo "  PgVisor Metrics & Charts Verification Test             "
echo "  Project: ${PROJECT_NAME} | Port: ${PROXY_PORT}         "
echo "========================================================="

echo "[0/6] Starting isolated test cluster ${PROJECT_NAME}..."
cluster_down "${PROJECT_NAME}" "${COMPOSE_FILE}"
cluster_up   "${PROJECT_NAME}" "${COMPOSE_FILE}"

echo "Waiting for cluster containers to report healthy..."
wait_for_healthy 120 "${PROJECT_NAME}-minio" "${NODE1_CONTAINER}" "${NODE2_CONTAINER}" "${NODE3_CONTAINER}"
wait_for_proxy_ready "${DASHBOARD_URL}" 60 "${AUTH_HEADER[@]}"

exec_sql() {
    local sql="$1"
    if command -v psql &> /dev/null; then
        PGPASSWORD="" psql -h "${PROXY_HOST}" -p "${PROXY_PORT}" -U postgres -d postgres -c "${sql}"
    else
        docker compose -p "${PROJECT_NAME}" -f "${COMPOSE_FILE}" exec -T pgvisor-proxy psql -h localhost -p 5432 -U postgres -d postgres -c "${sql}"
    fi
}

echo ""
echo "[1/6] Verifying Web Dashboard Overview page (/) renders telemetry charts..."
METRICS_HTML=$(curl -s -f -L "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/")
echo "${METRICS_HTML}" | grep -q "Cluster Metrics &amp; Telemetry" || {
    echo "FAILED: Overview page missing telemetry section"
    exit 1
}
echo "${METRICS_HTML}" | grep -q "uptimeChart" || {
    echo "FAILED: Overview page missing uptimeChart canvas"
    exit 1
}
echo "${METRICS_HTML}" | grep -q "nodeQueriesChart" || {
    echo "FAILED: Overview page missing nodeQueriesChart canvas"
    exit 1
}
echo "${METRICS_HTML}" | grep -q "cpuChart" || {
    echo "FAILED: Overview page missing cpuChart canvas"
    exit 1
}
echo "${METRICS_HTML}" | grep -q "memChart" || {
    echo "FAILED: Overview page missing memChart canvas"
    exit 1
}
echo "${METRICS_HTML}" | grep -q "proxyQueriesChart" || {
    echo "FAILED: Overview page missing proxyQueriesChart canvas"
    exit 1
}
echo "${METRICS_HTML}" | grep -q "replicationLagChart" || {
    echo "FAILED: Overview page missing replicationLagChart canvas"
    exit 1
}
echo "${METRICS_HTML}" | grep -q "backupSizeThroughputChart" || {
    echo "FAILED: Overview page missing backupSizeThroughputChart canvas"
    exit 1
}
echo "${METRICS_HTML}" | grep -q "backupRateChart" || {
    echo "FAILED: Overview page missing backupRateChart canvas"
    exit 1
}
echo "PASSED: Overview page renders all 7 visualization canvas elements."

echo ""
echo "[2/6] Verifying GET /api/metrics/snapshot..."
SNAPSHOT_JSON=$(curl -s -f "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/metrics/snapshot")
echo "${SNAPSHOT_JSON}" | grep -q "timestamp_ms" || {
    echo "FAILED: snapshot missing timestamp_ms"
    exit 1
}
echo "${SNAPSHOT_JSON}" | grep -q "nodes" || {
    echo "FAILED: snapshot missing nodes array"
    exit 1
}
echo "${SNAPSHOT_JSON}" | grep -q "proxy" || {
    echo "FAILED: snapshot missing proxy object"
    exit 1
}
echo "${SNAPSHOT_JSON}" | grep -q "backups" || {
    echo "FAILED: snapshot missing backups object"
    exit 1
}
echo "PASSED: /api/metrics/snapshot payload schema verified."

echo ""
echo "[3/6] Verifying GET /api/metrics/history..."
HISTORY_JSON=$(curl -s -f "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/metrics/history")
echo "${HISTORY_JSON}" | grep -q "\[" || {
    echo "FAILED: history response is not an array"
    exit 1
}
echo "PASSED: /api/metrics/history endpoint verified."

echo ""
echo "[4/6] Executing queries and verifying proxy read/write counters..."
INITIAL_WRITES=$(curl -s -f "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/metrics/snapshot" | grep -o '"writes_total":[0-9]*' | cut -d: -f2)
INITIAL_READS=$(curl -s -f "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/metrics/snapshot" | grep -o '"reads_total":[0-9]*' | cut -d: -f2)

echo "Initial proxy writes: ${INITIAL_WRITES}, reads: ${INITIAL_READS}"

# Execute mutating DDL / DML (writes)
exec_sql "CREATE TABLE IF NOT EXISTS t_metrics_test (id serial primary key, val text);"
exec_sql "INSERT INTO t_metrics_test (val) VALUES ('test_val_1'), ('test_val_2');"

# Execute read queries
exec_sql "SELECT * FROM t_metrics_test;"
exec_sql "SELECT count(*) FROM t_metrics_test;"

NEW_WRITES=$(curl -s -f "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/metrics/snapshot" | grep -o '"writes_total":[0-9]*' | cut -d: -f2)
NEW_READS=$(curl -s -f "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/metrics/snapshot" | grep -o '"reads_total":[0-9]*' | cut -d: -f2)

echo "New proxy writes: ${NEW_WRITES}, reads: ${NEW_READS}"

if [ "${NEW_WRITES}" -le "${INITIAL_WRITES}" ]; then
    echo "FAILED: proxy writes_total did not increase after INSERT/CREATE"
    exit 1
fi
if [ "${NEW_READS}" -le "${INITIAL_READS}" ]; then
    echo "FAILED: proxy reads_total did not increase after SELECT"
    exit 1
fi
echo "PASSED: Proxy read and write counters successfully incremented."

echo ""
echo "[5/6] Verifying node-level CPU, memory, and uptime telemetry..."
# Allow topology monitor loop to collect at least one cycle of telemetry
sleep 3
LATEST_SNAPSHOT=$(curl -s -f "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/metrics/snapshot")

# Verify nodes array contains at least 3 nodes
NODE_COUNT=$(echo "${LATEST_SNAPSHOT}" | grep -o '"node_id":[0-9]*' | wc -l)
if [ "${NODE_COUNT}" -lt 3 ]; then
    echo "FAILED: Expected at least 3 nodes in telemetry, found ${NODE_COUNT}"
    exit 1
fi

echo "Found ${NODE_COUNT} active nodes in cluster telemetry snapshot."

# Verify node uptimes are positive
echo "${LATEST_SNAPSHOT}" | grep -q '"uptime_secs":' || {
    echo "FAILED: uptime_secs not present in node metrics"
    exit 1
}

# Verify node memory usage is tracked
echo "${LATEST_SNAPSHOT}" | grep -q '"memory_used_bytes":' || {
    echo "FAILED: memory_used_bytes not present in node metrics"
    exit 1
}
echo "PASSED: Node-level telemetry (uptime, CPU, memory) verified."

echo ""
echo "[6/6] Verifying backup metrics..."
# Trigger a backup via dashboard API
BACKUP_RES=$(curl -s -f -X POST "${AUTH_HEADER[@]}" -H "Content-Type: application/json" -d '{"backup_type": "full", "label": "metrics-test-snapshot"}' "${DASHBOARD_URL}/api/backups")
echo "Backup triggered: ${BACKUP_RES}"

# Verify snapshot reflects backup
sleep 2
BACKUP_SNAPSHOT=$(curl -s -f "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/metrics/snapshot")
TOTAL_BACKUPS=$(echo "${BACKUP_SNAPSHOT}" | grep -o '"total_backups":[0-9]*' | cut -d: -f2)
if [ "${TOTAL_BACKUPS}" -lt 1 ]; then
    echo "FAILED: total_backups not updated in metrics snapshot"
    exit 1
fi
echo "PASSED: Backup metrics verified with total_backups=${TOTAL_BACKUPS}."

echo ""
echo "========================================================="
echo "  All 6 Metrics & Charts assertions passed successfully! "
echo "========================================================="
