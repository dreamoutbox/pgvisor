#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Cluster Internal Auth Security Verification Test
#
# Self-contained concurrent test profile using Docker Compose.
# Tests security of internal communication over the network:
#   1. Proxy -> Sidecar Communication:
#      - Bad guy unauthenticated requests -> 401 Unauthorized
#      - Bad guy forged random token requests -> 401 Unauthorized
#      - Bad guy wrong password requests -> 401 Unauthorized
#      - Bad guy forged fence/restore commands -> 401 Unauthorized
#      - Legitimate proxy requests with valid HMAC-SHA256 bearer token -> 200 OK
#      - Proxy container with valid secret monitors nodes successfully
#   2. Sidecar -> Sidecar Communication:
#      - Rogue sidecar unauthenticated repoint -> 401 Unauthorized
#      - Rogue sidecar forged token repoint -> 401 Unauthorized
#      - Rogue sidecar forged demote attempt -> 401 Unauthorized
#      - Legitimate peer sidecar requests with valid HMAC-SHA256 token -> 200 OK
# All output is clean plain text (no ANSI color escapes).
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
# shellcheck source=tests/lib/cluster.sh
source "${SCRIPT_DIR}/lib/cluster.sh"

readonly TEST_PROXY_PORT=7532
readonly TEST_DASHBOARD_PORT=10180
readonly TEST_MINIO_PORT=11100
readonly TEST_MINIO_CONSOLE=11101
readonly PROJECT_NAME="pgvisor-cluster-auth"
readonly COMPOSE_FILE="${REPO_ROOT}/composes/docker-compose.cluster-auth.yml"

readonly CLUSTER_SECRET="super-pgvisor-secret-cluster-auth-2026"
readonly HACKER_SECRET="bad-guy-forged-hacker-secret-666"

export PGVISOR_CLUSTER_SECRET="${CLUSTER_SECRET}"

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

# Helper to derive deterministic HMAC-SHA256 bearer token
derive_token() {
    local secret="$1"
    if command -v openssl &> /dev/null; then
        echo -n "pgvisor-internal-v1" | openssl dgst -sha256 -hmac "${secret}" | awk '{print $NF}'
    elif [ -x "${REPO_ROOT}/scripts/derive-auth-token.py" ]; then
        "${REPO_ROOT}/scripts/derive-auth-token.py" "${secret}"
    else
        echo "ERROR: openssl or scripts/derive-auth-token.py required for HMAC-SHA256 calculation" >&2
        exit 1
    fi
}

VALID_TOKEN="$(derive_token "${CLUSTER_SECRET}")"
FORGED_TOKEN="$(derive_token "${HACKER_SECRET}")"

trap cleanup EXIT

echo "========================================================="
echo "  PgVisor: Cluster Internal Auth Security Verification   "
echo "  Project: ${PROJECT_NAME} | Dashboard Port: ${TEST_DASHBOARD_PORT}"
echo "========================================================="

echo "[0/4] Starting isolated test cluster ${PROJECT_NAME} via Docker Compose..."
cluster_down "${PROJECT_NAME}" "${COMPOSE_FILE}"
cluster_up   "${PROJECT_NAME}" "${COMPOSE_FILE}"

echo "Waiting for cluster containers to report healthy..."
wait_for_healthy 120 "${PROJECT_NAME}-minio" "${NODE1_CONTAINER}" "${NODE2_CONTAINER}" "${NODE3_CONTAINER}"
wait_for_proxy_ready "${DASHBOARD_URL}" 60 "${AUTH_HEADER[@]}"

echo ""
echo "[1/4] Running Security Tests: Proxy -> Sidecar Communication"
echo "-------------------------------------------------------------"

# Test 1.1: Bad guy sends GET /control/status with NO Authorization header
echo -n "Test 1.1: Bad guy request with NO Authorization header -> "
CODE=$(docker exec -i "${PROXY_CONTAINER}" curl -s -o /dev/null -w "%{http_code}" \
    "http://${NODE1_CONTAINER}:8080/control/status")
if [ "${CODE}" = "401" ]; then
    echo "PASS (HTTP 401 Unauthorized correctly returned)"
else
    echo "FAIL (Expected HTTP 401, got ${CODE})" >&2
    exit 1
fi

# Test 1.2: Bad guy sends request with forged random bearer token
echo -n "Test 1.2: Bad guy request with forged random bearer token -> "
CODE=$(docker exec -i "${PROXY_CONTAINER}" curl -s -o /dev/null -w "%{http_code}" \
    -H "Authorization: Bearer bad_guy_forged_random_token_abcdef1234567890" \
    "http://${NODE1_CONTAINER}:8080/control/status")
if [ "${CODE}" = "401" ]; then
    echo "PASS (HTTP 401 Unauthorized correctly returned)"
else
    echo "FAIL (Expected HTTP 401, got ${CODE})" >&2
    exit 1
fi

# Test 1.3: Bad guy sends request with token derived from the WRONG password
echo -n "Test 1.3: Bad guy request with token from WRONG password -> "
CODE=$(docker exec -i "${PROXY_CONTAINER}" curl -s -o /dev/null -w "%{http_code}" \
    -H "Authorization: Bearer ${FORGED_TOKEN}" \
    "http://${NODE1_CONTAINER}:8080/control/status")
if [ "${CODE}" = "401" ]; then
    echo "PASS (HTTP 401 Unauthorized correctly returned)"
else
    echo "FAIL (Expected HTTP 401, got ${CODE})" >&2
    exit 1
fi

# Test 1.4: Bad guy malicious fence request with forged token
echo -n "Test 1.4: Bad guy malicious POST /control/fence with forged token -> "
CODE=$(docker exec -i "${PROXY_CONTAINER}" curl -s -o /dev/null -w "%{http_code}" -X POST \
    -H "Authorization: Bearer ${FORGED_TOKEN}" \
    "http://${NODE1_CONTAINER}:8080/control/fence")
if [ "${CODE}" = "401" ]; then
    echo "PASS (HTTP 401 Unauthorized correctly returned)"
else
    echo "FAIL (Expected HTTP 401, got ${CODE})" >&2
    exit 1
fi

# Test 1.5: Bad guy request with invalid authorization scheme (Basic auth)
echo -n "Test 1.5: Bad guy request with Basic auth scheme -> "
CODE=$(docker exec -i "${PROXY_CONTAINER}" curl -s -o /dev/null -w "%{http_code}" \
    -H "Authorization: Basic YWRtaW46cGFzc3dvcmQ=" \
    "http://${NODE1_CONTAINER}:8080/control/events")
if [ "${CODE}" = "401" ]; then
    echo "PASS (HTTP 401 Unauthorized correctly returned)"
else
    echo "FAIL (Expected HTTP 401, got ${CODE})" >&2
    exit 1
fi

# Test 1.6: Legitimate Proxy request with valid HMAC-SHA256 bearer token
echo -n "Test 1.6: Legitimate Proxy request with valid Authorization header -> "
CODE=$(docker exec -i "${PROXY_CONTAINER}" curl -s -o /dev/null -w "%{http_code}" \
    -H "Authorization: Bearer ${VALID_TOKEN}" \
    "http://${NODE1_CONTAINER}:8080/control/status")
if [ "${CODE}" = "200" ]; then
    echo "PASS (HTTP 200 OK successfully authenticated)"
else
    echo "FAIL (Expected HTTP 200, got ${CODE})" >&2
    exit 1
fi

echo ""
echo "[2/4] Running Security Tests: Sidecar -> Sidecar Communication"
echo "-------------------------------------------------------------"

# Test 2.1: Rogue sidecar unauthenticated repoint attempt
echo -n "Test 2.1: Rogue sidecar unauthenticated POST /control/repoint -> "
CODE=$(docker exec -i "${NODE2_CONTAINER}" curl -s -o /dev/null -w "%{http_code}" -X POST \
    -H "Content-Type: application/json" \
    -d '{"primary_conninfo": "host=evil-attacker.com port=5432 user=hacker"}' \
    "http://${NODE1_CONTAINER}:8080/control/repoint")
if [ "${CODE}" = "401" ]; then
    echo "PASS (HTTP 401 Unauthorized correctly returned)"
else
    echo "FAIL (Expected HTTP 401, got ${CODE})" >&2
    exit 1
fi

# Test 2.2: Rogue sidecar repoint attempt with forged token
echo -n "Test 2.2: Rogue sidecar POST /control/repoint with forged bearer token -> "
CODE=$(docker exec -i "${NODE2_CONTAINER}" curl -s -o /dev/null -w "%{http_code}" -X POST \
    -H "Authorization: Bearer ${FORGED_TOKEN}" \
    -H "Content-Type: application/json" \
    -d '{"primary_conninfo": "host=evil-attacker.com port=5432 user=hacker"}' \
    "http://${NODE1_CONTAINER}:8080/control/repoint")
if [ "${CODE}" = "401" ]; then
    echo "PASS (HTTP 401 Unauthorized correctly returned)"
else
    echo "FAIL (Expected HTTP 401, got ${CODE})" >&2
    exit 1
fi

# Test 2.3: Rogue sidecar malicious demote attempt with forged token
echo -n "Test 2.3: Rogue sidecar POST /control/demote with forged bearer token -> "
CODE=$(docker exec -i "${NODE2_CONTAINER}" curl -s -o /dev/null -w "%{http_code}" -X POST \
    -H "Authorization: Bearer ${FORGED_TOKEN}" \
    "http://${NODE1_CONTAINER}:8080/control/demote")
if [ "${CODE}" = "401" ]; then
    echo "PASS (HTTP 401 Unauthorized correctly returned)"
else
    echo "FAIL (Expected HTTP 401, got ${CODE})" >&2
    exit 1
fi

# Test 2.4: Rogue sidecar status probe during election with wrong token
echo -n "Test 2.4: Rogue sidecar peer status probe with forged bearer token -> "
CODE=$(docker exec -i "${NODE2_CONTAINER}" curl -s -o /dev/null -w "%{http_code}" \
    -H "Authorization: Bearer ${FORGED_TOKEN}" \
    "http://${NODE1_CONTAINER}:8080/control/status")
if [ "${CODE}" = "401" ]; then
    echo "PASS (HTTP 401 Unauthorized correctly returned)"
else
    echo "FAIL (Expected HTTP 401, got ${CODE})" >&2
    exit 1
fi

# Test 2.5: Legitimate Peer Sidecar probe with valid Authorization header
echo -n "Test 2.5: Legitimate Peer Sidecar probe with valid Authorization header -> "
CODE=$(docker exec -i "${NODE2_CONTAINER}" curl -s -o /dev/null -w "%{http_code}" \
    -H "Authorization: Bearer ${VALID_TOKEN}" \
    "http://${NODE1_CONTAINER}:8080/control/status")
if [ "${CODE}" = "200" ]; then
    echo "PASS (HTTP 200 OK successfully authenticated)"
else
    echo "FAIL (Expected HTTP 200, got ${CODE})" >&2
    exit 1
fi

# Test 2.6: Legitimate Peer Sidecar repoint broadcast with valid header
echo -n "Test 2.6: Legitimate Peer Sidecar POST /control/repoint with valid header -> "
CODE=$(docker exec -i "${NODE2_CONTAINER}" curl -s -o /dev/null -w "%{http_code}" -X POST \
    -H "Authorization: Bearer ${VALID_TOKEN}" \
    -H "Content-Type: application/json" \
    -d "{\"primary_conninfo\": \"host=${NODE1_CONTAINER} port=5432 user=postgres\"}" \
    "http://${NODE2_CONTAINER}:8080/control/repoint")
if [ "${CODE}" = "200" ]; then
    echo "PASS (HTTP 200 OK successfully accepted)"
else
    echo "FAIL (Expected HTTP 200, got ${CODE})" >&2
    exit 1
fi

echo ""
echo "[3/4] Verifying Legitimate Proxy Cluster Monitoring"
echo "-------------------------------------------------------------"

echo -n "Checking proxy dashboard node topology status -> "
DASH_NODES=$(curl -s ${AUTH_HEADER[@]+"${AUTH_HEADER[@]}"} "${DASHBOARD_URL}/api/status" 2>/dev/null || true)
if [ -n "${DASH_NODES}" ]; then
    echo "PASS (Dashboard accessible and responding)"
else
    echo "FAIL (Dashboard not responding at ${DASHBOARD_URL}/api/status)" >&2
    exit 1
fi

echo ""
echo "[4/4] Summary"
echo "========================================================="
echo "  All Cluster Communication Security Tests PASSED!       "
echo "  Docker Compose profile ${PROJECT_NAME} verified.       "
echo "  Proxy->Sidecar & Sidecar->Sidecar strictly secured.    "
echo "========================================================="
