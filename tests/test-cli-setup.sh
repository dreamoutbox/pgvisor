#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# End-to-End Integration Test for `pgvisor` CLI (setup -> up -> SQL -> restart -> down -> clean)
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
# shellcheck source=lib/helper.sh
source "${SCRIPT_DIR}/lib/helper.sh"

TMP_DIR="$(mktemp -d /tmp/pgvisor-cli-e2e.XXXXXX)"
PGVISOR_BIN="${REPO_ROOT}/target/debug/pgvisor"

cleanup() {
    if [ -x "${PGVISOR_BIN}" ] && [ -f "${TMP_DIR}/docker-compose.yml" ]; then
        "${PGVISOR_BIN}" -C "${TMP_DIR}" clean --yes >/dev/null 2>&1 || true
    fi
    rm -rf "${TMP_DIR}"
}
trap cleanup EXIT

echo "Building pgvisor CLI..."
cargo build --manifest-path "${REPO_ROOT}/Cargo.toml" -p pgvisor-cli

PROXY_PORT=35432
DASHBOARD_PORT=38080
S3_API_PORT=39000
S3_CONSOLE_PORT=39001

echo "Running pgvisor setup in isolated directory ${TMP_DIR}..."
"${PGVISOR_BIN}" -C "${TMP_DIR}" setup \
    --non-interactive \
    --image dreamoutbox/pgvisor:latest \
    --proxy-port "${PROXY_PORT}" \
    --dashboard-port "${DASHBOARD_PORT}" \
    --s3-api-port "${S3_API_PORT}" \
    --s3-console-port "${S3_CONSOLE_PORT}"

ENV_PERM="$(stat -c '%a' "${TMP_DIR}/.env")"
if [ "${ENV_PERM}" != "600" ]; then
    echo "ERROR: Expected .env permissions 600, got ${ENV_PERM}" >&2
    exit 1
fi

SCRIPT_PERM="$(stat -c '%a' "${TMP_DIR}/scripts/init-s3-bucket.sh")"
if [ "${SCRIPT_PERM}" != "755" ]; then
    echo "ERROR: Expected init-s3-bucket.sh permissions 755, got ${SCRIPT_PERM}" >&2
    exit 1
fi

POSTGRES_PASSWORD="$(grep '^POSTGRES_PASSWORD=' "${TMP_DIR}/.env" | cut -d'=' -f2-)"
CLUSTER_SECRET="$(grep '^PGVISOR_CLUSTER_SECRET=' "${TMP_DIR}/.env" | cut -d'=' -f2-)"
SESSION_SECRET="$(grep '^PGVISOR_API_SESSION_SECRET=' "${TMP_DIR}/.env" | cut -d'=' -f2-)"

if [ -z "${POSTGRES_PASSWORD}" ] || [ "${POSTGRES_PASSWORD}" = "postgres" ]; then
    echo "ERROR: Expected non-default POSTGRES_PASSWORD" >&2
    exit 1
fi
if [ -z "${CLUSTER_SECRET}" ] || [ -z "${SESSION_SECRET}" ]; then
    echo "ERROR: Expected non-empty cluster/session secrets" >&2
    exit 1
fi

echo "Starting cluster with pgvisor up..."
"${PGVISOR_BIN}" -C "${TMP_DIR}" up --timeout 120
"${PGVISOR_BIN}" -C "${TMP_DIR}" status

echo "Verifying SQL write and polling read through proxy..."
write_ok=false
for attempt in $(seq 1 15); do
    if PGPASSWORD="${POSTGRES_PASSWORD}" psql -h 127.0.0.1 -p "${PROXY_PORT}" -U postgres -d postgres -v ON_ERROR_STOP=1 -c \
        "CREATE TABLE IF NOT EXISTS cli_e2e (id INT PRIMARY KEY, val TEXT); INSERT INTO cli_e2e VALUES (1, 'ok') ON CONFLICT (id) DO UPDATE SET val = 'ok';" >/dev/null 2>&1; then
        write_ok=true
        break
    fi
    sleep 1
done
if [ "${write_ok}" != "true" ]; then
    echo "ERROR: Failed to execute initial SQL write through proxy" >&2
    exit 1
fi

read_val=""
for attempt in $(seq 1 10); do
    read_val="$(PGPASSWORD="${POSTGRES_PASSWORD}" psql -h 127.0.0.1 -p "${PROXY_PORT}" -U postgres -d postgres -t -A -c \
        "SELECT val FROM cli_e2e WHERE id = 1;" 2>/dev/null || true)"
    if [ "${read_val}" = "ok" ]; then
        break
    fi
    sleep 1
done
if [ "${read_val}" != "ok" ]; then
    echo "ERROR: Expected 'ok' from proxy read, got '${read_val}'" >&2
    exit 1
fi

echo "Verifying pgvisor restart..."
"${PGVISOR_BIN}" -C "${TMP_DIR}" restart --timeout 120

read_val=""
for attempt in $(seq 1 10); do
    read_val="$(PGPASSWORD="${POSTGRES_PASSWORD}" psql -h 127.0.0.1 -p "${PROXY_PORT}" -U postgres -d postgres -t -A -c \
        "SELECT val FROM cli_e2e WHERE id = 1;" 2>/dev/null || true)"
    if [ "${read_val}" = "ok" ]; then
        break
    fi
    sleep 1
done
if [ "${read_val}" != "ok" ]; then
    echo "ERROR: Expected 'ok' after restart, got '${read_val}'" >&2
    exit 1
fi

echo "Verifying pgvisor down and clean --yes..."
"${PGVISOR_BIN}" -C "${TMP_DIR}" down
"${PGVISOR_BIN}" -C "${TMP_DIR}" clean --yes

echo "PASS: test-cli-setup.sh completed successfully."
