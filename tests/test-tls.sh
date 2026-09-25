#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: TLS/SSL Auto-Setup Verification Test
#
# Tests end-to-end TLS encryption across PgVisor:
#   1. Automatic self-signed certificate generation on nodes and proxy
#   2. Key permissions verified (0600 on server.key)
#   3. postgresql.conf contains ssl = on
#   4. Plain unencrypted client connection rejected when TLS is required
#   5. Encrypted client connection (sslmode=require) connects and runs queries
#   6. openssl s_client -starttls postgres handshake verification
# All output is clean plain text (no ANSI color escapes).
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
# shellcheck source=tests/lib/cluster.sh
source "${SCRIPT_DIR}/lib/cluster.sh"

readonly TEST_PROXY_PORT=7632
readonly TEST_DASHBOARD_PORT=10280
readonly TEST_MINIO_PORT=11200
readonly TEST_MINIO_CONSOLE=11201
readonly PROJECT_NAME="pgvisor-tls"
readonly COMPOSE_FILE="${REPO_ROOT}/composes/docker-compose.tls.yml"

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
PROXY_CONTAINER="${PROJECT_NAME}-proxy"

if [ ! -f "${COMPOSE_FILE}" ]; then
    echo "Error: Compose file not found at ${COMPOSE_FILE}"
    echo "Run: ./scripts/generate-test-composes.sh"
    exit 1
fi

trap cleanup EXIT

echo "============================================================="
echo "  PgVisor TLS/SSL Mode Test: ${PROJECT_NAME}"
echo "============================================================="

# 1. Start cluster with TLS compose profile
echo "[1/6] Starting test cluster with TLS enabled..."
cluster_up "${PROJECT_NAME}" "${COMPOSE_FILE}"

echo "Waiting for containers to report healthy..."
wait_for_healthy 60 "${NODE1_CONTAINER}" "${NODE2_CONTAINER}" "${NODE3_CONTAINER}" "${PROXY_CONTAINER}"
wait_for_proxy_ready "${DASHBOARD_URL}" 30 "${AUTH_HEADER[@]}"
echo "All containers healthy and proxy dashboard ready."

# 2. Verify auto-generated certificates on PostgreSQL nodes
echo ""
echo "[2/6] Verifying auto-generated certificates on nodes..."
for node in "${NODE1_CONTAINER}" "${NODE2_CONTAINER}" "${NODE3_CONTAINER}"; do
    echo "Checking ${node}..."
    docker exec "${node}" test -f /var/lib/postgresql/data/pgdata/server.crt
    docker exec "${node}" test -f /var/lib/postgresql/data/pgdata/server.key
    
    # Check key permissions (must be 0600)
    key_perms=$(docker exec "${node}" stat -c "%a" /var/lib/postgresql/data/pgdata/server.key)
    if [ "${key_perms}" != "600" ]; then
        echo "FAIL: ${node} server.key permissions are ${key_perms}, expected 600" >&2
        exit 1
    fi
    
    # Verify postgresql.conf contains ssl = on
    docker exec "${node}" grep -q "^ssl = on" /var/lib/postgresql/data/pgdata/postgresql.conf
    echo "  ${node}: cert and key present, key perms=600, ssl=on in postgresql.conf"
done
echo "PASS: All PostgreSQL nodes have valid auto-generated TLS certificates and ssl=on."

# 3. Verify proxy certificate generation
echo ""
echo "[3/6] Verifying proxy certificate in volume..."
docker exec "${PROXY_CONTAINER}" test -f /var/lib/postgresql/tls/proxy.crt
docker exec "${PROXY_CONTAINER}" test -f /var/lib/postgresql/tls/proxy.key
proxy_key_perms=$(docker exec "${PROXY_CONTAINER}" stat -c "%a" /var/lib/postgresql/tls/proxy.key)
if [ "${proxy_key_perms}" != "600" ]; then
    echo "FAIL: proxy.key permissions are ${proxy_key_perms}, expected 600" >&2
    exit 1
fi
echo "PASS: Proxy certificate and key present with 600 permissions."

# 4. Verify unencrypted connection is rejected when TLS is required
echo ""
echo "[4/6] Verifying unencrypted connection rejection..."
plain_output=$(psql "sslmode=disable host=${PROXY_HOST} port=${PROXY_PORT} user=postgres dbname=postgres" -c "SELECT 1;" 2>&1 || true)
if echo "${plain_output}" | grep -qi "TLS/SSL is required"; then
    echo "PASS: Unencrypted connection rejected with expected TLS required error message:"
    echo "  ${plain_output}"
else
    echo "FAIL: Unencrypted connection was not rejected with expected error. Output:" >&2
    echo "${plain_output}" >&2
    exit 1
fi

# 5. Verify encrypted connection (sslmode=require) and query execution
echo ""
echo "[5/6] Verifying encrypted client queries (sslmode=require)..."
res=$(psql "sslmode=require host=${PROXY_HOST} port=${PROXY_PORT} user=postgres dbname=postgres" -t -A -c "SELECT 42 as answer;")
if [ "${res}" = "42" ]; then
    echo "PASS: Connected over TLS and received expected result '42'."
else
    echo "FAIL: Expected '42', got '${res}'" >&2
    exit 1
fi

# Test DDL and DML operations over TLS
psql "sslmode=require host=${PROXY_HOST} port=${PROXY_PORT} user=postgres dbname=postgres" << 'EOF' > /dev/null
DROP TABLE IF EXISTS tls_test_table;
CREATE TABLE tls_test_table (id INT PRIMARY KEY, secret_msg TEXT);
INSERT INTO tls_test_table VALUES (1, 'pgvisor-tls-verified');
EOF

val=$(psql "sslmode=require host=${PROXY_HOST} port=${PROXY_PORT} user=postgres dbname=postgres" -t -A -c "SELECT secret_msg FROM tls_test_table WHERE id = 1;")
if [ "${val}" = "pgvisor-tls-verified" ]; then
    echo "PASS: Table created, data inserted, and read back over TLS successfully: '${val}'."
else
    echo "FAIL: Expected 'pgvisor-tls-verified', got '${val}'" >&2
    exit 1
fi

# 6. Verify TLS handshake via openssl s_client
echo ""
echo "[6/6] Verifying TLS handshake using openssl s_client..."
if command -v openssl > /dev/null 2>&1; then
    ssl_info=$(openssl s_client -starttls postgres -connect "${PROXY_HOST}:${PROXY_PORT}" </dev/null 2>&1 || true)
    if echo "${ssl_info}" | grep -q -E "CN\s*=\s*pgvisor-proxy"; then
        echo "PASS: openssl s_client completed STARTTLS handshake and confirmed CN=pgvisor-proxy."
    else
        echo "FAIL: openssl s_client did not find CN=pgvisor-proxy. Output:" >&2
        echo "${ssl_info}" >&2
        exit 1
    fi
else
    echo "SKIP: openssl not installed on host, skipping openssl s_client check."
fi

echo ""
echo "============================================================="
echo "  TLS/SSL Auto-Setup Verification Test PASSED"
echo "============================================================="
