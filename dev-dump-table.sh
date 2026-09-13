#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Developer Table Dump Utility
#
# Dumps PostgreSQL user tables to disk under ./debug/* (formatted text and CSV)
# and displays table contents to stdout.
#
# All output is strictly clean plain text (no ANSI escape codes).
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Configuration with environment variable overrides
PROXY_HOST="${PGVISOR_HOST:-127.0.0.1}"
PROXY_PORT="${PGVISOR_PORT:-5432}"
PGUSER="${PGUSER:-postgres}"
PGDATABASE="${PGDATABASE:-postgres}"
NODE_CONTAINER="${PGVISOR_NODE_CONTAINER:-pgvisor-node1}"
OUT_DIR="${PGVISOR_DEBUG_DIR:-${SCRIPT_DIR}/debug}"
TARGET_TABLE=""
QUIET=false

# CLI Arguments
while [[ $# -gt 0 ]]; do
    case "$1" in
        --host=*)
            PROXY_HOST="${1#*=}"
            shift
            ;;
        --port=*)
            PROXY_PORT="${1#*=}"
            shift
            ;;
        --user=*)
            PGUSER="${1#*=}"
            shift
            ;;
        --db=*)
            PGDATABASE="${1#*=}"
            shift
            ;;
        --container=*)
            NODE_CONTAINER="${1#*=}"
            shift
            ;;
        --out-dir=*)
            OUT_DIR="${1#*=}"
            shift
            ;;
        --table=*)
            TARGET_TABLE="${1#*=}"
            shift
            ;;
        -q|--quiet)
            QUIET=true
            shift
            ;;
        -h|--help)
            echo "Usage: $0 [options]"
            echo ""
            echo "Dumps PostgreSQL user tables to formatted text and CSV under ./debug/*."
            echo ""
            echo "Options:"
            echo "  --host=HOST        PostgreSQL proxy host (default: 127.0.0.1)"
            echo "  --port=PORT        PostgreSQL proxy port (default: 5432)"
            echo "  --user=USER        PostgreSQL user (default: postgres)"
            echo "  --db=DB            PostgreSQL database (default: postgres)"
            echo "  --container=NAME   Docker container fallback for psql (default: pgvisor-node1)"
            echo "  --out-dir=DIR      Output directory for table dumps (default: ./debug)"
            echo "  --table=TABLE      Optional single table name to dump (default: all user tables)"
            echo "  -q, --quiet        Suppress printing table contents to stdout"
            echo "  -h, --help         Show this help message"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            echo "Run '$0 --help' for usage."
            exit 1
            ;;
    esac
done

# Ensure destination directory exists
mkdir -p "${OUT_DIR}"

# Helper to execute query via local psql or container fallback
run_psql_query() {
    local query="$1"
    local extra_flags="${2:-}"
    local output=""

    if command -v psql &> /dev/null; then
        # shellcheck disable=SC2086
        if output=$(PGPASSWORD="" psql -h "${PROXY_HOST}" -p "${PROXY_PORT}" -U "${PGUSER}" -d "${PGDATABASE}" ${extra_flags} -c "${query}" 2>&1); then
            echo "${output}"
            return 0
        fi
    fi

    # Fallback to docker exec if local psql connection fails or is unavailable
    if docker ps --format '{{.Names}}' 2>/dev/null | grep -q "^${NODE_CONTAINER}$"; then
        # shellcheck disable=SC2086
        if output=$(docker exec -i "${NODE_CONTAINER}" psql -U "${PGUSER}" -d "${PGDATABASE}" ${extra_flags} -c "${query}" 2>&1); then
            echo "${output}"
            return 0
        fi
    fi

    echo "Query execution failed: ${output}" >&2
    return 1
}

# Resolve list of user tables
TABLES=()
if [ -n "${TARGET_TABLE}" ]; then
    TABLES=("${TARGET_TABLE}")
else
    DISCOVERY_SQL="SELECT table_name FROM information_schema.tables WHERE table_schema NOT IN ('pg_catalog', 'information_schema') AND table_type = 'BASE TABLE' ORDER BY table_name;"
    RAW_TABLES=$(run_psql_query "${DISCOVERY_SQL}" "-t -A")
    while IFS= read -r line; do
        line=$(echo "${line}" | tr -d '\r')
        if [ -n "${line}" ]; then
            TABLES+=("${line}")
        fi
    done <<< "${RAW_TABLES}"
fi

TIMESTAMP_UTC=$(date -u +"%Y-%m-%d %H:%M:%S UTC")
SUMMARY_FILE="${OUT_DIR}/summary.txt"

{
    echo "============================================================="
    echo "  PgVisor Table Dump Summary"
    echo "  Timestamp: ${TIMESTAMP_UTC}"
    echo "  Target   : ${PROXY_HOST}:${PROXY_PORT}/${PGDATABASE}"
    echo "============================================================="
    echo ""
} > "${SUMMARY_FILE}"

if [ "${#TABLES[@]}" -eq 0 ]; then
    echo "No user tables found in database '${PGDATABASE}'." | tee -a "${SUMMARY_FILE}"
    exit 0
fi

echo "Dumping ${#TABLES[@]} user table(s) to '${OUT_DIR}'..."

for tbl in "${TABLES[@]}"; do
    TXT_FILE="${OUT_DIR}/${tbl}.txt"
    CSV_FILE="${OUT_DIR}/${tbl}.csv"

    # 1. Aligned human-readable text dump
    run_psql_query "SELECT * FROM \"${tbl}\";" "" > "${TXT_FILE}"

    # 2. Machine-readable CSV dump
    run_psql_query "SELECT * FROM \"${tbl}\";" "--csv" > "${CSV_FILE}" 2>/dev/null || \
        run_psql_query "COPY (SELECT * FROM \"${tbl}\") TO STDOUT WITH CSV HEADER;" "" > "${CSV_FILE}" 2>/dev/null || true

    # 3. Row count
    ROW_COUNT=$(run_psql_query "SELECT count(*) FROM \"${tbl}\";" "-t -A" | tr -d '\r' || echo "unknown")

    BYTES_TXT=$(wc -c < "${TXT_FILE}" 2>/dev/null || echo 0)
    BYTES_CSV=$(wc -c < "${CSV_FILE}" 2>/dev/null || echo 0)

    echo "  -> Table '${tbl}': ${ROW_COUNT} rows (txt: ${BYTES_TXT} bytes, csv: ${BYTES_CSV} bytes)" | tee -a "${SUMMARY_FILE}"

    if [ "${QUIET}" = false ]; then
        echo ""
        echo "-------------------------------------------------------------"
        echo "  Table: ${tbl} (${ROW_COUNT} rows)"
        echo "-------------------------------------------------------------"
        cat "${TXT_FILE}"
        echo ""
    fi
done

echo "Dump completed successfully. All artifacts stored in: ${OUT_DIR}" | tee -a "${SUMMARY_FILE}"
