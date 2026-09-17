#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Quick TLS/SSL Connection Verification Script
#
# Usage:
#   ./scripts/test-tls.sh [host] [port] [user] [dbname]
#
# Defaults:
#   host:   localhost
#   port:   5432
#   user:   postgres
#   dbname: postgres
# ==============================================================================

PG_HOST="${1:-${PGVISOR_HOST:-localhost}}"
PG_PORT="${2:-${PGVISOR_PORT:-5432}}"
PG_USER="${3:-${PGUSER:-postgres}}"
PG_DB="${4:-${PGDATABASE:-postgres}}"

echo "============================================================="
echo "  PgVisor TLS/SSL Connection Tester"
echo "  Connecting to ${PG_HOST}:${PG_PORT} (user: ${PG_USER}, db: ${PG_DB})"
echo "============================================================="

# 1. Test TLS Handshake via openssl if available
echo "[1/3] Testing PostgreSQL STARTTLS handshake via openssl..."
if command -v openssl > /dev/null 2>&1; then
    ssl_output=$(openssl s_client -starttls postgres -connect "${PG_HOST}:${PG_PORT}" </dev/null 2>&1 || true)
    if echo "${ssl_output}" | grep -q "SSL-Session"; then
        cipher=$(echo "${ssl_output}" | grep -i "Cipher    :" | tr -s ' ' || echo "Cipher negotiated")
        proto=$(echo "${ssl_output}" | grep -i "Protocol  :" | tr -s ' ' || echo "TLS")
        echo "  PASS: TLS handshake succeeded"
        echo "  ${proto}"
        echo "  ${cipher}"
    else
        echo "  WARN: openssl STARTTLS handshake failed or unencrypted"
    fi
else
    echo "  SKIP: openssl command not found on host"
fi

# 2. Test encrypted psql connection (sslmode=require)
echo ""
echo "[2/3] Testing encrypted connection via psql (sslmode=require)..."
if command -v psql > /dev/null 2>&1; then
    query_result=$(psql "sslmode=require host=${PG_HOST} port=${PG_PORT} user=${PG_USER} dbname=${PG_DB}" -t -A -c "SELECT 'TLS connection successful! Answer=' || (21*2);" 2>&1)
    echo "  Result: ${query_result}"
    if echo "${query_result}" | grep -q "TLS connection successful"; then
        echo "  PASS: psql connected over TLS and executed query"
    else
        echo "  FAIL: Query did not return expected output" >&2
        exit 1
    fi
else
    echo "  SKIP: psql command not found on host"
fi

# 3. Test unencrypted connection behavior (sslmode=disable)
echo ""
echo "[3/3] Checking unencrypted connection behavior (sslmode=disable)..."
if command -v psql > /dev/null 2>&1; then
    plain_output=$(psql "sslmode=disable host=${PG_HOST} port=${PG_PORT} user=${PG_USER} dbname=${PG_DB}" -t -A -c "SELECT 1;" 2>&1 || true)
    if echo "${plain_output}" | grep -qi "required"; then
        echo "  INFO: Plain unencrypted connection rejected as expected (server requires TLS)"
    else
        echo "  INFO: Plain unencrypted connection accepted (server permits fallback)"
    fi
fi

echo ""
echo "============================================================="
echo "  TLS/SSL connection test completed successfully."
echo "============================================================="
