#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Docker Swarm DinD Manual Leader Switchover Verification Test
#
# Tests graceful leader demotion, standby promotion, replica repointing,
# proxy pool reconnection, and write continuity in Docker Swarm via DinD.
# All output is clean plain text.
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

# shellcheck source=tests/lib/swarm-helper.sh
source "${SCRIPT_DIR}/lib/swarm-helper.sh"

readonly TEST_PROXY_PORT=7834
readonly TEST_DASHBOARD_PORT=10482
readonly TEST_MINIO_PORT=11420
readonly DIND_CONTAINER="pgvisor-swarm-switchover-dind"
readonly STACK_NAME="pgvisor-swarm-switchover"
readonly STACK_FILE="${REPO_ROOT}/stacks/docker-stack.yml"
readonly TABLE_NAME="t_swarm_switchover"

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
echo "  PgVisor Docker Swarm Switchover Verification Test      "
echo "  Stack: ${STACK_NAME} | Proxy Port: ${PROXY_PORT}       "
echo "========================================================="

echo "[1/8] Initializing Docker-in-Docker Swarm cluster..."
swarm_up "${DIND_CONTAINER}" "${PROXY_PORT}" "${TEST_DASHBOARD_PORT}" "${TEST_MINIO_PORT}"

echo "[2/8] Deploying PgVisor Swarm stack '${STACK_NAME}'..."
swarm_stack_deploy "${STACK_NAME}" "${STACK_FILE}"

echo "Waiting for MinIO service to become healthy..."
swarm_wait_for_healthy 60 minio

echo "Ensuring S3 backup bucket exists..."
swarm_init_s3_bucket "${STACK_NAME}"

echo "Waiting for PostgreSQL cluster nodes to report healthy..."
swarm_wait_for_healthy 120 pgvisor-node1 pgvisor-node2 pgvisor-node3
swarm_wait_for_proxy_ready "${DASHBOARD_URL}" 60 "${AUTH_HEADER[@]}"

echo ""
echo "[3/8] Verifying baseline cluster connectivity and topology..."
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
echo "[4/8] Seeding test table '${TABLE_NAME}' with baseline record 'alpha_t0'..."
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
echo "[5/8] Initiating manual switchover: promoting Node #2 via Dashboard API..."
SWITCHOVER_RESP=$(curl -s -X POST \
    -H "Content-Type: application/json" \
    "${AUTH_HEADER[@]}" \
    -d '{"target_node_id": 2}' \
    "${DASHBOARD_URL}/api/cluster/switchover")

echo "  API Response: ${SWITCHOVER_RESP}"
STATUS_VAL=$(echo "${SWITCHOVER_RESP}" | json_extract "status")
NEW_LEADER_ID=$(echo "${SWITCHOVER_RESP}" | json_extract "new_leader_id")

if [[ "${STATUS_VAL}" != "ok" || "${NEW_LEADER_ID}" != "2" ]]; then
    echo "ERROR: Switchover API failed: ${SWITCHOVER_RESP}"
    exit 1
fi
echo "+ Switchover API accepted! Node #2 designated as new primary."

echo ""
echo "[6/8] Verifying Node #2 became read-write primary and old leader demoted..."
NODE2_PROMOTED=false
for attempt in $(seq 1 15); do
    NODE2_RECOVERY=$(swarm_run_node_sql pgvisor-node2 "SELECT pg_is_in_recovery();" || echo "err")
    if [[ "${NODE2_RECOVERY}" == "f" ]]; then
        NODE2_PROMOTED=true
        break
    fi
    sleep 1
done

if [[ "${NODE2_PROMOTED}" != "true" ]]; then
    echo "ERROR: Node #2 did not promote to read-write primary within timeout."
    exit 1
fi
echo "+ Confirmed: pgvisor-node2 is now operating as read-write primary (pg_is_in_recovery=f)."

echo "Verifying old leader pgvisor-node1 demoted to standby..."
NODE1_DEMOTED=false
for attempt in $(seq 1 20); do
    NODE1_RECOVERY=$(swarm_run_node_sql pgvisor-node1 "SELECT pg_is_in_recovery();" || echo "err")
    if [[ "${NODE1_RECOVERY}" == "t" ]]; then
        NODE1_DEMOTED=true
        break
    fi
    sleep 1
done

if [[ "${NODE1_DEMOTED}" != "true" ]]; then
    echo "ERROR: Old leader pgvisor-node1 did not demote to standby."
    exit 1
fi
echo "+ Confirmed: pgvisor-node1 demoted to standby replica (pg_is_in_recovery=t)."

echo ""
echo "[7/8] Executing post-switchover write 'beta_t1' via proxy..."
WRITE_SUCCESS=false
for attempt in $(seq 1 15); do
    if swarm_run_proxy_sql "INSERT INTO ${TABLE_NAME} (val) VALUES ('beta_t1');" 1 >/dev/null 2>&1; then
        WRITE_SUCCESS=true
        break
    fi
    sleep 1
done

if [[ "${WRITE_SUCCESS}" != "true" ]]; then
    echo "ERROR: Post-switchover write failed via proxy"
    exit 1
fi
echo "+ Post-switchover write 'beta_t1' succeeded via proxy."

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

echo ""
echo "[8/8] Verifying replication across standbys (Node #1 and Node #3)..."
for node in "pgvisor-node1" "pgvisor-node3"; do
    node_count=""
    for attempt in $(seq 1 10); do
        node_count=$(swarm_run_node_sql "${node}" "SELECT count(*) FROM ${TABLE_NAME};")
        if [[ "${node_count}" == "2" ]]; then
            break
        fi
        sleep 1
    done

    if [[ "${node_count}" != "2" ]]; then
        echo "ERROR: Standby ${node} failed to replicate data, got count: ${node_count}"
        exit 1
    fi
    echo "+ Standby ${node} replicated data successfully: count=2."
done

echo ""
echo "Docker Swarm Switchover test finished successfully!"
