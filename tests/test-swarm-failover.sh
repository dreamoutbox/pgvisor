#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Docker Swarm DinD High Availability Failover Verification Test
#
# Tests leader failure detection, standby consensus election, auto-promotion,
# proxy pool reconnection, and write continuity in Docker Swarm via DinD.
# All output is clean plain text.
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

# shellcheck source=tests/lib/swarm-helper.sh
source "${SCRIPT_DIR}/lib/swarm-helper.sh"

readonly TEST_PROXY_PORT=7833
readonly TEST_DASHBOARD_PORT=10481
readonly TEST_MINIO_PORT=11410
readonly DIND_CONTAINER="pgvisor-swarm-failover-dind"
readonly STACK_NAME="pgvisor-swarm-failover"
readonly STACK_FILE="${REPO_ROOT}/stacks/docker-stack.yml"
readonly TABLE_NAME="t_swarm_failover"

PROXY_HOST="${PGVISOR_HOST:-localhost}"
PROXY_PORT="${PGVISOR_PORT:-${TEST_PROXY_PORT}}"
DASHBOARD_URL="http://localhost:${TEST_DASHBOARD_PORT}"

trap swarm_cleanup EXIT

echo "========================================================="
echo "  PgVisor Docker Swarm Failover Verification Test (DinD) "
echo "  Stack: ${STACK_NAME} | Proxy Port: ${PROXY_PORT}       "
echo "========================================================="

echo "[1/7] Initializing Docker-in-Docker Swarm cluster..."
swarm_up "${DIND_CONTAINER}" "${PROXY_PORT}" "${TEST_DASHBOARD_PORT}" "${TEST_MINIO_PORT}"

echo "[2/7] Deploying PgVisor Swarm stack '${STACK_NAME}'..."
swarm_stack_deploy "${STACK_NAME}" "${STACK_FILE}"

echo "Waiting for MinIO service to become healthy..."
swarm_wait_for_healthy 60 minio

echo "Ensuring S3 backup bucket exists..."
swarm_init_s3_bucket "${STACK_NAME}"

echo "Waiting for PostgreSQL cluster nodes to report healthy..."
swarm_wait_for_healthy 120 pgvisor-node1 pgvisor-node2 pgvisor-node3
swarm_wait_for_proxy_ready "${DASHBOARD_URL}" 60

echo ""
echo "[3/7] Verifying baseline cluster connectivity and topology..."
if ! swarm_run_proxy_sql "SELECT 1;" >/dev/null; then
    echo "ERROR: Cannot connect to PostgreSQL cluster via proxy at ${PROXY_HOST}:${PROXY_PORT}"
    exit 1
fi
echo "+ PostgreSQL cluster proxy is reachable."

NODE1_RECOVERY=$(swarm_run_node_sql pgvisor-node1 "SELECT pg_is_in_recovery();")
NODE2_RECOVERY=$(swarm_run_node_sql pgvisor-node2 "SELECT pg_is_in_recovery();")
NODE3_RECOVERY=$(swarm_run_node_sql pgvisor-node3 "SELECT pg_is_in_recovery();")

if [[ "${NODE1_RECOVERY}" != "f" ]]; then
    echo "ERROR: pgvisor-node1 must be primary (pg_is_in_recovery=f), got: ${NODE1_RECOVERY}"
    exit 1
fi
if [[ "${NODE2_RECOVERY}" != "t" || "${NODE3_RECOVERY}" != "t" ]]; then
    echo "ERROR: Standby replicas node2 and node3 must be in recovery mode (pg_is_in_recovery=t)"
    exit 1
fi
echo "+ Baseline verified: node1 is primary, node2 and node3 are standbys."

echo ""
echo "[4/7] Seeding test table '${TABLE_NAME}' with baseline record 'alpha_t0'..."
swarm_run_proxy_sql "DROP TABLE IF EXISTS ${TABLE_NAME};" >/dev/null
swarm_run_proxy_sql "CREATE TABLE ${TABLE_NAME} (id SERIAL PRIMARY KEY, val TEXT NOT NULL, created_at TIMESTAMPTZ DEFAULT NOW());" >/dev/null
swarm_run_proxy_sql "INSERT INTO ${TABLE_NAME} (val) VALUES ('alpha_t0');" >/dev/null

COUNT_T0=""
for attempt in $(seq 1 10); do
    COUNT_T0=$(swarm_run_proxy_sql "SELECT count(*) FROM ${TABLE_NAME};")
    if [[ "${COUNT_T0}" == "1" ]]; then
        break
    fi
    sleep 1
done

if [[ "${COUNT_T0}" != "1" ]]; then
    echo "ERROR: Expected 1 row in ${TABLE_NAME}, got: ${COUNT_T0}"
    exit 1
fi
echo "+ Baseline row seeded via proxy: val='alpha_t0'"

echo ""
echo "[5/7] Simulating leader failure: scaling service pgvisor-node1 to 0..."
swarm_scale_service pgvisor-node1 0
echo "+ pgvisor-node1 scaled to 0 in Swarm."

echo ""
echo "[6/7] Waiting for consensus election and standby promotion..."
NEW_LEADER=""
SURVIVING_STANDBY=""
MAX_WAIT=30
ELAPSED=0

while [[ ${ELAPSED} -lt ${MAX_WAIT} ]]; do
    NODE2_RECOVERY=$(swarm_run_node_sql pgvisor-node2 "SELECT pg_is_in_recovery();" || echo "err")
    if [[ "${NODE2_RECOVERY}" == "f" ]]; then
        NEW_LEADER="pgvisor-node2"
        SURVIVING_STANDBY="pgvisor-node3"
        break
    fi

    NODE3_RECOVERY=$(swarm_run_node_sql pgvisor-node3 "SELECT pg_is_in_recovery();" || echo "err")
    if [[ "${NODE3_RECOVERY}" == "f" ]]; then
        NEW_LEADER="pgvisor-node3"
        SURVIVING_STANDBY="pgvisor-node2"
        break
    fi

    sleep 1
    ELAPSED=$((ELAPSED + 1))
done

if [[ -z "${NEW_LEADER}" ]]; then
    echo "ERROR: Neither standby promoted within ${MAX_WAIT}s" >&2
    exit 1
fi
echo "+ Consensus promotion successful: ${NEW_LEADER} is new primary (pg_is_in_recovery=f)!"
echo "+ Surviving standby is: ${SURVIVING_STANDBY}"

echo ""
echo "[7/7] Executing post-failover write 'beta_t1' via proxy..."
WRITE_SUCCESS=false
for attempt in $(seq 1 15); do
    if swarm_run_proxy_sql "INSERT INTO ${TABLE_NAME} (val) VALUES ('beta_t1');" 1 >/dev/null 2>&1; then
        WRITE_SUCCESS=true
        break
    fi
    sleep 1
done

if [[ "${WRITE_SUCCESS}" != "true" ]]; then
    echo "ERROR: Post-failover write failed via proxy after 15 attempts"
    exit 1
fi
echo "+ Post-failover write 'beta_t1' succeeded via proxy."

# Verify row count via proxy
COUNT_T1=""
for attempt in $(seq 1 10); do
    COUNT_T1=$(swarm_run_proxy_sql "SELECT count(*) FROM ${TABLE_NAME};")
    if [[ "${COUNT_T1}" == "2" ]]; then
        break
    fi
    sleep 1
done

if [[ "${COUNT_T1}" != "2" ]]; then
    echo "ERROR: Expected 2 rows in ${TABLE_NAME} via proxy, got: ${COUNT_T1}"
    exit 1
fi
echo "+ Row count verified via proxy: 2 rows (alpha_t0, beta_t1)."

# Verify replication to surviving standby
STANDBY_COUNT=""
for attempt in $(seq 1 10); do
    STANDBY_COUNT=$(swarm_run_node_sql "${SURVIVING_STANDBY}" "SELECT count(*) FROM ${TABLE_NAME};")
    if [[ "${STANDBY_COUNT}" == "2" ]]; then
        break
    fi
    sleep 1
done

if [[ "${STANDBY_COUNT}" != "2" ]]; then
    echo "ERROR: Surviving standby ${SURVIVING_STANDBY} did not replicate data, got count: ${STANDBY_COUNT}"
    exit 1
fi
echo "+ Surviving standby ${SURVIVING_STANDBY} replicated data successfully: count=2."

echo ""
echo "Docker Swarm Failover test finished successfully!"
