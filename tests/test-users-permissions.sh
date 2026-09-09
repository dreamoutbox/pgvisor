#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Database Users & Role Permissions Verification Test
#
# Tests the following 4 scenarios:
#   1. Create user via dashboard API -> verify user exists in pg_roles
#   2. Grant table privilege -> verify has_table_privilege() returns true
#   3. Revoke privilege -> verify has_table_privilege() returns false
#   4. Drop user via dashboard API -> verify user no longer in pg_roles
#
# All output is strictly clean plain text (no ANSI escape codes).
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

# Pre-defined test port & project constants
readonly TEST_PROXY_PORT=6332
readonly TEST_DASHBOARD_PORT=8980
readonly TEST_MINIO_PORT=9900
readonly TEST_MINIO_CONSOLE=9901
readonly PROJECT_NAME="pgvisor-users-permissions"
readonly COMPOSE_FILE="${REPO_ROOT}/composes/docker-compose.users-permissions.yml"

PROXY_HOST="${PGVISOR_HOST:-localhost}"
PROXY_PORT="${PGVISOR_PORT:-${TEST_PROXY_PORT}}"
DASHBOARD_URL="${PGVISOR_DASHBOARD_URL:-http://localhost:${TEST_DASHBOARD_PORT}}"
TEST_USER="test_app_user"
TEST_TABLE="t_perm_demo"

ADMIN_TOKEN="${PGVISOR_ADMIN_TOKEN:-postgres}"
AUTH_HEADER=()
if [ -n "${ADMIN_TOKEN}" ]; then
    AUTH_HEADER=(-H "Authorization: Bearer ${ADMIN_TOKEN}")
fi

NODE1_CONTAINER="pgvisor-users-permissions-node1"
NODE2_CONTAINER="pgvisor-users-permissions-node2"
NODE3_CONTAINER="pgvisor-users-permissions-node3"
PROXY_CONTAINER="pgvisor-users-permissions-proxy"

cleanup() {
    echo "Tearing down cluster ${PROJECT_NAME}..."
    docker compose -p "${PROJECT_NAME}" -f "${COMPOSE_FILE}" down -v --remove-orphans > /dev/null 2>&1 || true
}
trap cleanup EXIT

echo "========================================================="
echo "  PgVisor Users & Permissions Verification Test          "
echo "  Project: ${PROJECT_NAME} | Port: ${PROXY_PORT}         "
echo "========================================================="

echo "[0/5] Starting isolated test cluster ${PROJECT_NAME}..."
docker compose -p "${PROJECT_NAME}" -f "${COMPOSE_FILE}" down -v --remove-orphans > /dev/null 2>&1 || true
docker compose -p "${PROJECT_NAME}" -f "${COMPOSE_FILE}" up -d

echo "Waiting for cluster nodes to report healthy..."
until [ "$(docker inspect -f '{{.State.Health.Status}}' "${NODE1_CONTAINER}" 2>/dev/null)" = "healthy" ] && \
      [ "$(docker inspect -f '{{.State.Health.Status}}' "${NODE2_CONTAINER}" 2>/dev/null)" = "healthy" ] && \
      [ "$(docker inspect -f '{{.State.Health.Status}}' "${NODE3_CONTAINER}" 2>/dev/null)" = "healthy" ]; do
    sleep 1
done

echo "Waiting for proxy to become ready..."
until curl -s "${DASHBOARD_URL}/api/status" > /dev/null 2>&1; do
    sleep 1
done

run_sql() {
    local query="$1"
    for attempt in 1 2 3 4 5; do
        if command -v psql &> /dev/null; then
            if output=$(PGPASSWORD="" psql -h "${PROXY_HOST}" -p "${PROXY_PORT}" -U postgres -d postgres -t -A -c "${query}" 2>/dev/null); then
                echo "${output}"
                return 0
            fi
        else
            if output=$(docker compose -p "${PROJECT_NAME}" -f "${COMPOSE_FILE}" exec -T pgvisor-proxy psql -h localhost -p 5432 -U postgres -d postgres -t -A -c "${query}" 2>/dev/null); then
                echo "${output}"
                return 0
            fi
        fi
        sleep 1
    done
    echo "${output:-}"
    return 1
}

echo "Creating baseline test table ${TEST_TABLE}..."
run_sql "CREATE TABLE IF NOT EXISTS ${TEST_TABLE} (id int, val text); INSERT INTO ${TEST_TABLE} VALUES (1, 'initial');"

# Scenario 1: Create user via API
echo "[1/5] Creating user '${TEST_USER}' via dashboard API..."
CREATE_RESP=$(curl -s -X POST "${DASHBOARD_URL}/api/users" \
    "${AUTH_HEADER[@]}" \
    -H "Content-Type: application/json" \
    -d "{\"name\": \"${TEST_USER}\", \"password\": \"secret123\", \"login\": true, \"createdb\": false, \"connection_limit\": 10}")

echo "Response: ${CREATE_RESP}"

echo "Asserting user '${TEST_USER}' exists in pg_roles..."
USER_EXISTS="0"
for attempt in 1 2 3 4 5; do
    USER_EXISTS=$(run_sql "SELECT count(*) FROM pg_roles WHERE rolname = '${TEST_USER}';" || echo "0")
    if [ "${USER_EXISTS}" = "1" ]; then
        break
    fi
    sleep 1
done
if [ "${USER_EXISTS}" != "1" ]; then
    echo "FAIL: Expected 1 role with name '${TEST_USER}', found: ${USER_EXISTS}"
    exit 1
fi
echo "SUCCESS: User '${TEST_USER}' verified in pg_roles."

# Scenario 2: Grant table privilege via API
echo "[2/5] Granting SELECT privilege on ${TEST_TABLE} to ${TEST_USER}..."
GRANT_RESP=$(curl -s -X POST "${DASHBOARD_URL}/api/users/${TEST_USER}/privileges" \
    "${AUTH_HEADER[@]}" \
    -H "Content-Type: application/json" \
    -d "{\"table_name\": \"${TEST_TABLE}\", \"schema\": \"public\", \"privilege\": \"select\", \"grant\": true}")

echo "Response: ${GRANT_RESP}"

echo "Asserting has_table_privilege returns true..."
CAN_SELECT="f"
for attempt in 1 2 3 4 5; do
    CAN_SELECT=$(run_sql "SELECT has_table_privilege('${TEST_USER}', '${TEST_TABLE}', 'SELECT');" || echo "f")
    if [ "${CAN_SELECT}" = "t" ]; then
        break
    fi
    sleep 1
done
if [ "${CAN_SELECT}" != "t" ]; then
    echo "FAIL: Expected has_table_privilege to be 't', got: ${CAN_SELECT}"
    exit 1
fi
echo "SUCCESS: SELECT privilege successfully granted and verified."

# Scenario 3: Revoke table privilege via API
echo "[3/5] Revoking SELECT privilege on ${TEST_TABLE} from ${TEST_USER}..."
REVOKE_RESP=$(curl -s -X POST "${DASHBOARD_URL}/api/users/${TEST_USER}/privileges" \
    "${AUTH_HEADER[@]}" \
    -H "Content-Type: application/json" \
    -d "{\"table_name\": \"${TEST_TABLE}\", \"schema\": \"public\", \"privilege\": \"select\", \"grant\": false}")

echo "Response: ${REVOKE_RESP}"

echo "Asserting has_table_privilege returns false..."
CAN_SELECT_AFTER="t"
for attempt in 1 2 3 4 5; do
    CAN_SELECT_AFTER=$(run_sql "SELECT has_table_privilege('${TEST_USER}', '${TEST_TABLE}', 'SELECT');" || echo "t")
    if [ "${CAN_SELECT_AFTER}" = "f" ]; then
        break
    fi
    sleep 1
done
if [ "${CAN_SELECT_AFTER}" != "f" ]; then
    echo "FAIL: Expected has_table_privilege to be 'f', got: ${CAN_SELECT_AFTER}"
    exit 1
fi
echo "SUCCESS: SELECT privilege successfully revoked and verified."

# Scenario 4: Drop user via API
echo "[4/5] Dropping user '${TEST_USER}' via dashboard API..."
DROP_RESP=$(curl -s -X DELETE "${DASHBOARD_URL}/api/users/${TEST_USER}" \
    "${AUTH_HEADER[@]}")

echo "Response: ${DROP_RESP}"

echo "Asserting user '${TEST_USER}' is removed from pg_roles..."
USER_REMAIN="1"
for attempt in 1 2 3 4 5; do
    USER_REMAIN=$(run_sql "SELECT count(*) FROM pg_roles WHERE rolname = '${TEST_USER}';" || echo "1")
    if [ "${USER_REMAIN}" = "0" ]; then
        break
    fi
    sleep 1
done
if [ "${USER_REMAIN}" != "0" ]; then
    echo "FAIL: Expected 0 roles with name '${TEST_USER}', found: ${USER_REMAIN}"
    exit 1
fi
echo "SUCCESS: User '${TEST_USER}' successfully dropped and absent from pg_roles."

# Scenario 5: Verify dashboard users page loads cleanly
echo "[5/5] Checking GET /users page rendering..."
USERS_PAGE_STATUS=$(curl -s -o /dev/null -w "%{http_code}" "${DASHBOARD_URL}/users" "${AUTH_HEADER[@]}")
if [ "${USERS_PAGE_STATUS}" != "200" ]; then
    echo "FAIL: GET /users returned status ${USERS_PAGE_STATUS}"
    exit 1
fi
echo "SUCCESS: GET /users rendered with HTTP 200 OK."

echo ""
echo "========================================================="
echo "  All Users & Permissions verification tests passed!     "
echo "========================================================="
