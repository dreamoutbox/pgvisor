#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Docker Swarm DinD Scale Out / Add New Node Verification Test
#
# Tests dynamically scaling out the cluster by adding a 4th node (pgvisor-node4)
# to a live running Swarm stack, verifying pg_basebackup initial cloning,
# streaming WAL replication, and proxy routing in Docker Swarm via DinD.
# All output is clean plain text.
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

# shellcheck source=tests/lib/swarm-helper.sh
source "${SCRIPT_DIR}/lib/swarm-helper.sh"

readonly TEST_PROXY_PORT=7835
readonly TEST_DASHBOARD_PORT=10483
readonly TEST_MINIO_PORT=11430
readonly DIND_CONTAINER="pgvisor-swarm-add-node-dind"
readonly STACK_NAME="pgvisor-swarm-add-node"
readonly STACK_BASE_FILE="${REPO_ROOT}/stacks/docker-stack.yml"
readonly STACK_ADD_NODE_FILE="${REPO_ROOT}/stacks/docker-stack.add-node.yml"
readonly TABLE_NAME="t_swarm_scale"

PROXY_HOST="${PGVISOR_HOST:-localhost}"
PROXY_PORT="${PGVISOR_PORT:-${TEST_PROXY_PORT}}"
DASHBOARD_URL="http://localhost:${TEST_DASHBOARD_PORT}"
ADMIN_TOKEN="${PGVISOR_ADMIN_TOKEN:-postgres}"
AUTH_HEADER=()
if [ -n "${ADMIN_TOKEN}" ]; then
    AUTH_HEADER=(-H "Authorization: Bearer ${ADMIN_TOKEN}")
fi

trap swarm_cleanup EXIT

echo "========================================================="
echo "  PgVisor Docker Swarm Scale Out: Add Node Test (DinD)   "
echo "  Stack: ${STACK_NAME} | Proxy Port: ${PROXY_PORT}       "
echo "========================================================="

echo "[1/8] Initializing Docker-in-Docker Swarm cluster..."
swarm_up "${DIND_CONTAINER}" "${PROXY_PORT}" "${TEST_DASHBOARD_PORT}" "${TEST_MINIO_PORT}"

echo "[2/8] Deploying baseline PgVisor Swarm stack (3 nodes + proxy)..."
swarm_stack_deploy "${STACK_NAME}" "${STACK_BASE_FILE}"

echo "Waiting for MinIO service to become healthy..."
swarm_wait_for_healthy 60 minio

echo "Ensuring S3 backup bucket exists..."
swarm_init_s3_bucket "${STACK_NAME}"

echo "Waiting for baseline PostgreSQL cluster nodes to report healthy..."
swarm_wait_for_healthy 120 pgvisor-node1 pgvisor-node2 pgvisor-node3
swarm_wait_for_proxy_ready "${DASHBOARD_URL}" 60 "${AUTH_HEADER[@]}"

echo ""
echo "[3/8] Verifying baseline cluster connectivity and replication topology..."
if ! swarm_run_proxy_sql "SELECT 1;" >/dev/null; then
    echo "ERROR: Cannot connect to PostgreSQL cluster via proxy at ${PROXY_HOST}:${PROXY_PORT}"
    exit 1
fi
echo "+ Baseline cluster reachable via proxy."

INITIAL_WAL_SENDERS=$(swarm_run_node_sql pgvisor-node1 "SELECT count(*) FROM pg_stat_replication;")
echo "+ Baseline active replication connections on leader: ${INITIAL_WAL_SENDERS}"
if [[ "${INITIAL_WAL_SENDERS}" -lt 2 ]]; then
    echo "ERROR: Expected at least 2 standbys connected to leader, got: ${INITIAL_WAL_SENDERS}"
    exit 1
fi

echo ""
echo "[4/8] Seeding test table '${TABLE_NAME}' with baseline record 't0_pre_scale'..."
swarm_run_proxy_sql "DROP TABLE IF EXISTS ${TABLE_NAME};" >/dev/null
swarm_run_proxy_sql "CREATE TABLE ${TABLE_NAME} (id SERIAL PRIMARY KEY, val TEXT NOT NULL, created_at TIMESTAMPTZ DEFAULT NOW());" >/dev/null
swarm_run_proxy_sql "INSERT INTO ${TABLE_NAME} (val) VALUES ('t0_pre_scale');" >/dev/null

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
echo "+ Baseline record seeded via proxy: val='t0_pre_scale'"

echo ""
echo "[5/8] Dynamically deploying 4th node (pgvisor-node4) to Swarm stack..."
export PRIMARY_CONNINFO="host=pgvisor-node1 port=5432 user=postgres"
swarm_stack_deploy "${STACK_NAME}" "${STACK_ADD_NODE_FILE}"

echo "Waiting for pgvisor-node4 to become healthy (pg_basebackup clone + startup)..."
swarm_wait_for_healthy 90 pgvisor-node4
echo "+ pgvisor-node4 container is healthy."

echo ""
echo "[6/8] Verifying pgvisor-node4 sidecar status and recovery mode..."
ST4=$(swarm_get_sidecar_status pgvisor-node4)
ROLE4=$(echo "${ST4}" | json_extract "role")
STATUS4=$(echo "${ST4}" | json_extract "status")

echo "+ pgvisor-node4 sidecar reports: role='${ROLE4}', status='${STATUS4}'"
if [[ "${ROLE4}" != "standby" || "${STATUS4}" != "running" ]]; then
    echo "ERROR: Unexpected status for pgvisor-node4: role='${ROLE4}', status='${STATUS4}'"
    exit 1
fi

NODE4_RECOVERY=$(swarm_run_node_sql pgvisor-node4 "SELECT pg_is_in_recovery();")
if [[ "${NODE4_RECOVERY}" != "t" ]]; then
    echo "ERROR: pgvisor-node4 must be in recovery mode (pg_is_in_recovery=t), got: ${NODE4_RECOVERY}"
    exit 1
fi
echo "+ pgvisor-node4 confirmed in PostgreSQL recovery mode (pg_is_in_recovery=t)."

# Verify historical data cloned from leader
HISTORICAL_VAL=$(swarm_run_node_sql pgvisor-node4 "SELECT val FROM ${TABLE_NAME} WHERE val = 't0_pre_scale';")
if [[ "${HISTORICAL_VAL}" != "t0_pre_scale" ]]; then
    echo "ERROR: pgvisor-node4 failed to clone historical record, got: '${HISTORICAL_VAL}'"
    exit 1
fi
echo "+ Historical data cloned successfully: found 't0_pre_scale' on pgvisor-node4."

echo ""
echo "[7/8] Verifying live streaming replication to new node (pgvisor-node4)..."
swarm_run_proxy_sql "INSERT INTO ${TABLE_NAME} (val) VALUES ('t1_post_scale');" >/dev/null

LIVE_VAL=""
for attempt in $(seq 1 15); do
    LIVE_VAL=$(swarm_run_node_sql pgvisor-node4 "SELECT val FROM ${TABLE_NAME} WHERE val = 't1_post_scale';")
    if [[ "${LIVE_VAL}" == "t1_post_scale" ]]; then
        break
    fi
    sleep 1
done

if [[ "${LIVE_VAL}" != "t1_post_scale" ]]; then
    echo "ERROR: pgvisor-node4 failed to replicate live record 't1_post_scale'"
    exit 1
fi
echo "+ Live streaming replication verified: pgvisor-node4 replicated 't1_post_scale'."

echo ""
echo "[8/8] Verifying updated cluster replication connection count on leader..."
UPDATED_WAL_SENDERS=""
for attempt in $(seq 1 10); do
    UPDATED_WAL_SENDERS=$(swarm_run_node_sql pgvisor-node1 "SELECT count(*) FROM pg_stat_replication;")
    if [[ "${UPDATED_WAL_SENDERS}" -ge 3 ]]; then
        break
    fi
    sleep 1
done

echo "+ Active replication connections on leader after scale-out: ${UPDATED_WAL_SENDERS}"
if [[ "${UPDATED_WAL_SENDERS}" -lt 3 ]]; then
    echo "ERROR: Expected at least 3 active WAL senders on leader, got: ${UPDATED_WAL_SENDERS}"
    exit 1
fi
echo "+ Cluster scale-out confirmed: 3 active standby replicas streaming from leader."

echo ""
echo "Docker Swarm Scale Out (Add Node) test finished successfully!"
