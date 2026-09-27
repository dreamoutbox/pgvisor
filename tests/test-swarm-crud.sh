#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Docker Swarm DinD Basic CRUD Verification Test
#
# Tests basic CRUD operations, proxy routing, and replication across
# standby nodes in an isolated Docker Swarm cluster running via DinD.
# All output is clean plain text.
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

# shellcheck source=tests/lib/swarm-helper.sh
source "${SCRIPT_DIR}/lib/swarm-helper.sh"

readonly TEST_PROXY_PORT=7832
readonly TEST_DASHBOARD_PORT=10480
readonly TEST_MINIO_PORT=11400
readonly DIND_CONTAINER="pgvisor-swarm-crud-dind"
readonly STACK_NAME="pgvisor-swarm-crud"
readonly STACK_FILE="${REPO_ROOT}/stacks/docker-stack.yml"
readonly TABLE_NAME="t_swarm_crud"

PROXY_HOST="${PGVISOR_HOST:-localhost}"
PROXY_PORT="${PGVISOR_PORT:-${TEST_PROXY_PORT}}"
DASHBOARD_URL="http://localhost:${TEST_DASHBOARD_PORT}"

trap swarm_cleanup EXIT

echo "========================================================="
echo "  PgVisor Docker Swarm CRUD Verification Test (DinD)     "
echo "  Stack: ${STACK_NAME} | Proxy Port: ${PROXY_PORT}       "
echo "========================================================="

echo "[1/6] Initializing Docker-in-Docker Swarm cluster..."
swarm_up "${DIND_CONTAINER}" "${PROXY_PORT}" "${TEST_DASHBOARD_PORT}" "${TEST_MINIO_PORT}"

echo "[2/6] Deploying PgVisor Swarm stack '${STACK_NAME}'..."
swarm_stack_deploy "${STACK_NAME}" "${STACK_FILE}"

echo "Waiting for MinIO service to become healthy..."
swarm_wait_for_healthy 60 minio

echo "Ensuring S3 backup bucket exists..."
swarm_init_s3_bucket "${STACK_NAME}"

echo "Waiting for PostgreSQL cluster nodes to report healthy..."
swarm_wait_for_healthy 120 pgvisor-node1 pgvisor-node2 pgvisor-node3
swarm_wait_for_proxy_ready "${DASHBOARD_URL}" 60

echo ""
echo "[3/6] Verifying baseline cluster connectivity and topology..."
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
echo "+ Baseline verified: node1 is primary, node2 and node3 are standby replicas."

echo ""
echo "[4/6] Executing CREATE TABLE and INSERT operations via proxy..."
swarm_run_proxy_sql "DROP TABLE IF EXISTS ${TABLE_NAME};" >/dev/null
swarm_run_proxy_sql "CREATE TABLE ${TABLE_NAME} (id SERIAL PRIMARY KEY, name TEXT NOT NULL, val INT NOT NULL, created_at TIMESTAMPTZ DEFAULT NOW());" >/dev/null
swarm_run_proxy_sql "INSERT INTO ${TABLE_NAME} (name, val) VALUES ('Alice', 100), ('Bob', 200), ('Charlie', 300);" >/dev/null

# Polling retry loop for read verification across replicas
COUNT_INSERT=""
for attempt in $(seq 1 10); do
    COUNT_INSERT=$(swarm_run_proxy_sql "SELECT count(*) FROM ${TABLE_NAME};")
    if [[ "${COUNT_INSERT}" == "3" ]]; then
        break
    fi
    sleep 1
done

if [[ "${COUNT_INSERT}" != "3" ]]; then
    echo "ERROR: Expected 3 rows after INSERT, got: ${COUNT_INSERT}"
    exit 1
fi
echo "+ INSERT verified: 3 rows created."

echo ""
echo "[5/6] Executing UPDATE and verifying replication across standby nodes..."
swarm_run_proxy_sql "UPDATE ${TABLE_NAME} SET val = 999 WHERE name = 'Alice';" >/dev/null

UPDATED_VAL=""
for attempt in $(seq 1 10); do
    UPDATED_VAL=$(swarm_run_proxy_sql "SELECT val FROM ${TABLE_NAME} WHERE name = 'Alice';")
    if [[ "${UPDATED_VAL}" == "999" ]]; then
        break
    fi
    sleep 1
done

if [[ "${UPDATED_VAL}" != "999" ]]; then
    echo "ERROR: Expected updated val=999 for Alice, got: ${UPDATED_VAL}"
    exit 1
fi
echo "+ UPDATE verified via proxy: Alice val=999."

# Direct verification on standby replicas to confirm replication
echo "Verifying replication of UPDATE directly on standbys..."
NODE2_VAL=""
NODE3_VAL=""
for attempt in $(seq 1 10); do
    NODE2_VAL=$(swarm_run_node_sql pgvisor-node2 "SELECT val FROM ${TABLE_NAME} WHERE name = 'Alice';")
    NODE3_VAL=$(swarm_run_node_sql pgvisor-node3 "SELECT val FROM ${TABLE_NAME} WHERE name = 'Alice';")
    if [[ "${NODE2_VAL}" == "999" && "${NODE3_VAL}" == "999" ]]; then
        break
    fi
    sleep 1
done

if [[ "${NODE2_VAL}" != "999" || "${NODE3_VAL}" != "999" ]]; then
    echo "ERROR: Standby replication failed. node2 val: '${NODE2_VAL}', node3 val: '${NODE3_VAL}'"
    exit 1
fi
echo "+ Direct standby assertion: node2 and node3 replicated update."

echo ""
echo "[6/6] Executing DELETE and verifying final row count..."
swarm_run_proxy_sql "DELETE FROM ${TABLE_NAME} WHERE name = 'Charlie';" >/dev/null

FINAL_COUNT=""
for attempt in $(seq 1 10); do
    FINAL_COUNT=$(swarm_run_proxy_sql "SELECT count(*) FROM ${TABLE_NAME};")
    if [[ "${FINAL_COUNT}" == "2" ]]; then
        break
    fi
    sleep 1
done

if [[ "${FINAL_COUNT}" != "2" ]]; then
    echo "ERROR: Expected 2 rows after DELETE, got: ${FINAL_COUNT}"
    exit 1
fi
echo "+ DELETE verified: 2 rows remaining."

echo ""
echo "Docker Swarm CRUD test finished successfully!"
