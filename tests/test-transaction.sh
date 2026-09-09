#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Transaction SQL Test (Self-Contained Concurrent Test Profile)
#
# Verifies correct proxy behavior for BEGIN/COMMIT, BEGIN/ROLLBACK, and
# error-mid-transaction recovery:
#   1. BEGIN...COMMIT: committed rows are visible after commit.
#   2. BEGIN...ROLLBACK: rolled-back row is absent.
#   3. Error mid-tx + ROLLBACK: partial writes do not persist.
#
# All output is strictly clean plain text (no ANSI escape codes).
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
SQL_FILE="${REPO_ROOT}/scripts/test-transaction.sql"

# Pre-defined test port & project constants
readonly TEST_PROXY_PORT=6432
readonly TEST_DASHBOARD_PORT=9080
readonly TEST_MINIO_PORT=10000
readonly TEST_MINIO_CONSOLE=10001
readonly PROJECT_NAME="pgvisor-transaction"
readonly COMPOSE_FILE="${REPO_ROOT}/composes/docker-compose.transaction.yml"

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

if [ ! -f "${SQL_FILE}" ]; then
    echo "Error: SQL fixture not found at ${SQL_FILE}"
    exit 1
fi

if [ ! -f "${COMPOSE_FILE}" ]; then
    echo "Error: Compose file not found at ${COMPOSE_FILE}"
    echo "Run: ./scripts/generate-test-composes.sh"
    exit 1
fi

cleanup() {
    echo "Tearing down cluster ${PROJECT_NAME}..."
    docker compose -p "${PROJECT_NAME}" -f "${COMPOSE_FILE}" down -v --remove-orphans > /dev/null 2>&1 || true
}
trap cleanup EXIT

# Helper: run a query through the proxy with up to 5 retries to absorb replication lag.
run_sql() {
    local query="$1"
    local output=""
    for attempt in 1 2 3 4 5; do
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
echo "  PgVisor Transaction SQL Test"
echo "  Project: ${PROJECT_NAME} | Proxy port: ${PROXY_PORT}"
echo "========================================================="

echo "[0/4] Starting isolated test cluster ${PROJECT_NAME}..."
docker compose -p "${PROJECT_NAME}" -f "${COMPOSE_FILE}" down -v --remove-orphans > /dev/null 2>&1 || true
docker compose -p "${PROJECT_NAME}" -f "${COMPOSE_FILE}" up -d

echo "Waiting for cluster nodes to report healthy..."
until [ "$(docker inspect -f '{{.State.Health.Status}}' "${NODE1_CONTAINER}" 2>/dev/null)" = "healthy" ] && \
      [ "$(docker inspect -f '{{.State.Health.Status}}' "${NODE2_CONTAINER}" 2>/dev/null)" = "healthy" ] && \
      [ "$(docker inspect -f '{{.State.Health.Status}}' "${NODE3_CONTAINER}" 2>/dev/null)" = "healthy" ]; do
    sleep 1
done

echo "Waiting for proxy to become ready..."
until curl -s "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/status" > /dev/null 2>&1; do
    sleep 1
done

# ---------------------------------------------------------------
# Run the SQL fixture (creates table, exercises all 3 scenarios,
# drops table). psql errors mid-transaction are expected (scenario 3)
# so we do NOT use set -e for this invocation.
# ---------------------------------------------------------------
echo "[1/4] Running SQL fixture ${SQL_FILE}..."
if command -v psql &> /dev/null; then
    PGPASSWORD="" psql -h "${PROXY_HOST}" -p "${PROXY_PORT}" -U postgres -d postgres \
        --set ON_ERROR_STOP=off \
        -f "${SQL_FILE}" || true
else
    docker compose -p "${PROJECT_NAME}" -f "${COMPOSE_FILE}" exec -T pgvisor-proxy \
        psql -h localhost -p 5432 -U postgres -d postgres \
        --set ON_ERROR_STOP=off \
        -f /scripts/test-transaction.sql || true
fi

# ---------------------------------------------------------------
# The SQL fixture drops the table at the end. We re-run each
# scenario in isolation via run_sql for precise assertion.
# ---------------------------------------------------------------
echo "[2/4] Re-running scenarios for precise assertion..."

# --- Scenario 1: BEGIN...COMMIT ---
echo "  Scenario 1: BEGIN...COMMIT"
run_sql "DROP TABLE IF EXISTS txn_assert;" > /dev/null
run_sql "CREATE TABLE txn_assert (id SERIAL PRIMARY KEY, val TEXT NOT NULL);" > /dev/null
run_sql "BEGIN; INSERT INTO txn_assert (val) VALUES ('alpha'); INSERT INTO txn_assert (val) VALUES ('beta'); COMMIT;" > /dev/null

COMMITTED_COUNT=""
for attempt in 1 2 3 4 5; do
    COMMITTED_COUNT=$(run_sql "SELECT count(*) FROM txn_assert;" || echo "0")
    if [ "${COMMITTED_COUNT}" = "2" ]; then
        break
    fi
    sleep 1
done
if [ "${COMMITTED_COUNT}" != "2" ]; then
    echo "FAIL [Scenario 1]: Expected 2 committed rows after COMMIT, got: ${COMMITTED_COUNT}"
    exit 1
fi
echo "  SUCCESS: Scenario 1 passed — committed rows visible (count=${COMMITTED_COUNT})."

# --- Scenario 2: BEGIN...ROLLBACK ---
echo "  Scenario 2: BEGIN...ROLLBACK"
run_sql "BEGIN; INSERT INTO txn_assert (val) VALUES ('should-vanish'); ROLLBACK;" > /dev/null

ROLLED_BACK=""
for attempt in 1 2 3 4 5; do
    ROLLED_BACK=$(run_sql "SELECT count(*) FROM txn_assert WHERE val = 'should-vanish';" || echo "1")
    if [ "${ROLLED_BACK}" = "0" ]; then
        break
    fi
    sleep 1
done
if [ "${ROLLED_BACK}" != "0" ]; then
    echo "FAIL [Scenario 2]: Expected 0 rows after ROLLBACK, got: ${ROLLED_BACK}"
    exit 1
fi
echo "  SUCCESS: Scenario 2 passed — rolled-back row absent (count=${ROLLED_BACK})."

# --- Scenario 3: Error mid-transaction + ROLLBACK ---
echo "  Scenario 3: error mid-transaction + ROLLBACK"
# Use ON_ERROR_STOP=off so psql continues after the division-by-zero error.
if command -v psql &> /dev/null; then
    PGPASSWORD="" psql -h "${PROXY_HOST}" -p "${PROXY_PORT}" -U postgres -d postgres \
        --set ON_ERROR_STOP=off -t -A \
        -c "BEGIN; INSERT INTO txn_assert (val) VALUES ('pre-error'); SELECT 1/0; ROLLBACK;" > /dev/null 2>&1 || true
else
    docker compose -p "${PROJECT_NAME}" -f "${COMPOSE_FILE}" exec -T pgvisor-proxy \
        psql -h localhost -p 5432 -U postgres -d postgres \
        --set ON_ERROR_STOP=off -t -A \
        -c "BEGIN; INSERT INTO txn_assert (val) VALUES ('pre-error'); SELECT 1/0; ROLLBACK;" > /dev/null 2>&1 || true
fi

ERROR_ROW=""
for attempt in 1 2 3 4 5; do
    ERROR_ROW=$(run_sql "SELECT count(*) FROM txn_assert WHERE val = 'pre-error';" || echo "1")
    if [ "${ERROR_ROW}" = "0" ]; then
        break
    fi
    sleep 1
done
if [ "${ERROR_ROW}" != "0" ]; then
    echo "FAIL [Scenario 3]: Expected 0 rows after error+ROLLBACK, got: ${ERROR_ROW}"
    exit 1
fi
echo "  SUCCESS: Scenario 3 passed — pre-error row absent after error+ROLLBACK (count=${ERROR_ROW})."

# Cleanup assertion table
run_sql "DROP TABLE IF EXISTS txn_assert;" > /dev/null

echo "[3/4] All transaction assertions passed."

echo "[4/4] Verifying proxy dashboard still reports healthy..."
STATUS_CODE=$(curl -s -o /dev/null -w "%{http_code}" "${AUTH_HEADER[@]}" "${DASHBOARD_URL}/api/status")
if [ "${STATUS_CODE}" != "200" ]; then
    echo "FAIL: Dashboard /api/status returned ${STATUS_CODE}"
    exit 1
fi
echo "  SUCCESS: Dashboard healthy (HTTP ${STATUS_CODE})."

echo ""
echo "========================================================="
echo "  All PgVisor Transaction SQL tests passed!"
echo "========================================================="
