#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Developer Transaction Inspection Utility
#
# Inspects PostgreSQL Write-Ahead Log (WAL) records and correlates them with
# server logs and audit events to display:
# - Transaction ID (XID)
# - Commit / Abort timestamp (UTC)
# - Commit status (COMMIT / ABORT / IN_PROGRESS)
# - Log Sequence Number (LSN)
# - Executed SQL statement or physical WAL operations
#
# Works with live Docker containers or offline WAL directories (e.g. S3 snapshots).
# All output is strictly clean plain text (no ANSI escape codes).
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PYTHON_PARSER="${SCRIPT_DIR}/scripts/lib/parse-transactions.py"

CONTAINER=""
TARGET_DIR=""
TARGET_WAL=""
SERVER_LOGS=""
AUDIT_LOGS=""
FILTER_STATUS="all"
ROW_LIMIT=""
SHOW_WAL_OPS=false
MAX_WIDTH=""
OUTPUT_JSON=false

TMP_LOG_FILE=""
TMP_WALDUMP_FILE=""

# ==============================================================================
# cleanup
# Cleans up temporary files generated during transaction inspection.
#
# Arguments:
#   None
#
# Example usage:
#   trap cleanup EXIT
# ==============================================================================
cleanup() {
    if [[ -n "${TMP_LOG_FILE}" && -f "${TMP_LOG_FILE}" ]]; then
        rm -f "${TMP_LOG_FILE}"
    fi
    if [[ -n "${TMP_WALDUMP_FILE}" && -f "${TMP_WALDUMP_FILE}" ]]; then
        rm -f "${TMP_WALDUMP_FILE}"
    fi
}
trap cleanup EXIT

# ==============================================================================
# usage
# Prints usage guide and exits.
#
# Arguments:
#   None
#
# Example usage:
#   usage
# ==============================================================================
usage() {
    cat << EOF
Usage: $0 [options]

Inspects PostgreSQL transactions (XID, timestamp, statement) from WAL and logs.

Target Options (pick one, default: auto-detect leader container):
  -c, --container=NAME    Target Docker container (e.g. pgvisor-node2)
  -d, --dir=DIR           Target offline directory containing WAL files
  -w, --wal=FILE          Inspect a single specific WAL file

Log Correlation Options:
  -l, --logs=FILE         PostgreSQL server log file to correlate SQL text
  -a, --audit=FILE        Audit batch JSON file to correlate high-level SQL text

Filter and Formatting Options:
  -s, --status=STATUS     Filter by transaction status: all, commit, abort (default: all)
  --wal-ops, --all        Include internal transactions with only physical WAL ops (Heap INSERT/MOD)
  -m, --max-width=N       Maximum statement column width (default: 100, 0 for unlimited)
  -n, --limit=N           Limit the number of transactions displayed
  -j, --json              Output results in JSON format instead of plain-text table
  -h, --help              Show this help message

Example usage:
  # Inspect live cluster leader:
  $0

  # Inspect specific node container:
  $0 --container=pgvisor-node2

  # Inspect offline WAL files from an S3 bucket snapshot:
  $0 --dir=./debug-s3-bucket-snapshot/clusters/pgvisor-cluster/wal

  # Inspect specific WAL file with audit log correlation:
  $0 --wal=./000000010000000000000004 --audit=./audit_batch_00000001.json

  # Output in JSON:
  $0 --json
EOF
    exit 0
}

# Parse command line options
while [[ $# -gt 0 ]]; do
    case "$1" in
        -c|--container)
            CONTAINER="$2"
            shift 2
            ;;
        --container=*)
            CONTAINER="${1#*=}"
            shift
            ;;
        -d|--dir)
            TARGET_DIR="$2"
            shift 2
            ;;
        --dir=*)
            TARGET_DIR="${1#*=}"
            shift
            ;;
        -w|--wal)
            TARGET_WAL="$2"
            shift 2
            ;;
        --wal=*)
            TARGET_WAL="${1#*=}"
            shift
            ;;
        -l|--logs)
            SERVER_LOGS="$2"
            shift 2
            ;;
        --logs=*)
            SERVER_LOGS="${1#*=}"
            shift
            ;;
        -a|--audit)
            AUDIT_LOGS="$2"
            shift 2
            ;;
        --audit=*)
            AUDIT_LOGS="${1#*=}"
            shift
            ;;
        -s|--status)
            FILTER_STATUS="$2"
            shift 2
            ;;
        --status=*)
            FILTER_STATUS="${1#*=}"
            shift
            ;;
        --wal-ops|--all)
            SHOW_WAL_OPS=true
            shift
            ;;
        -m|--max-width)
            MAX_WIDTH="$2"
            shift 2
            ;;
        --max-width=*)
            MAX_WIDTH="${1#*=}"
            shift
            ;;
        -n|--limit)
            ROW_LIMIT="$2"
            shift 2
            ;;
        --limit=*)
            ROW_LIMIT="${1#*=}"
            shift
            ;;
        -j|--json)
            OUTPUT_JSON=true
            shift
            ;;
        -h|--help)
            usage
            ;;
        *)
            echo "ERROR: Unknown option '$1'" >&2
            echo "Run '$0 --help' for usage instructions." >&2
            exit 1
            ;;
    esac
done

# Validate python parser exists
if [[ ! -f "${PYTHON_PARSER}" ]]; then
    echo "ERROR: Python parser helper not found at '${PYTHON_PARSER}'" >&2
    exit 1
fi

# ==============================================================================
# detect_live_container
# Auto-detects the active leader or healthy pgvisor node container.
#
# Arguments:
#   None
#
# Returns:
#   Container name printed to stdout.
#
# Example usage:
#   target_node="$(detect_live_container)"
# ==============================================================================
detect_live_container() {
    # Check running containers
    local candidates=("pgvisor-node1" "pgvisor-node2" "pgvisor-node3")
    local found=""

    for node in "${candidates[@]}"; do
        if docker ps --format '{{.Names}}' | grep -q "^${node}$"; then
            # Check if this node is responsive and primary
            local is_standby
            is_standby=$(docker exec "${node}" psql -U postgres -h /tmp -t -c "SELECT pg_is_in_recovery();" 2>/dev/null | tr -d ' ' || true)
            if [[ "${is_standby}" == "f" ]]; then
                echo "${node}"
                return 0
            elif [[ -z "${found}" ]]; then
                found="${node}"
            fi
        fi
    done

    if [[ -n "${found}" ]]; then
        echo "${found}"
        return 0
    fi

    # Fallback: check any pgvisor container
    local any_container
    any_container=$(docker ps --filter "name=pgvisor-node" --format '{{.Names}}' | head -n 1 || true)
    if [[ -n "${any_container}" ]]; then
        echo "${any_container}"
        return 0
    fi

    return 1
}

# Auto-detect target mode if none specified
if [[ -z "${CONTAINER}" && -z "${TARGET_DIR}" && -z "${TARGET_WAL}" ]]; then
    if detected="$(detect_live_container)"; then
        CONTAINER="${detected}"
    else
        echo "ERROR: No container, directory, or WAL file specified and no active pgvisor containers found." >&2
        echo "Use --container=NAME, --dir=DIR, or --wal=FILE. See --help." >&2
        exit 1
    fi
fi

# Create temporary files for pipeline
TMP_WALDUMP_FILE=$(mktemp "/tmp/pgvisor-waldump-XXXXXX.log")

# ==============================================================================
# dump_from_container
# Extracts WAL records and server logs from a running Docker container.
#
# Arguments:
#   $1 - Container name
#   $2 - Output file for raw waldump
#
# Example usage:
#   dump_from_container "pgvisor-node2" "/tmp/waldump.log"
# ==============================================================================
dump_from_container() {
    local container="$1"
    local out_file="$2"

    local wal_dir="/var/lib/postgresql/data/pgdata/pg_wal"

    # Find WAL segment files in container
    local wal_files
    wal_files=$(docker exec "${container}" find "${wal_dir}" -maxdepth 1 -type f 2>/dev/null | grep -E '/[0-9A-Fa-f]{24}(\.partial)?$' | sort || true)

    if [[ -z "${wal_files}" ]]; then
        echo "WARNING: No WAL segments found in container ${container} at ${wal_dir}" >&2
        return 0
    fi

    # Run pg_waldump on each segment
    while IFS= read -r segment; do
        if [[ -n "${segment}" ]]; then
            docker exec "${container}" pg_waldump "${segment}" >> "${out_file}" 2>/dev/null || true
        fi
    done <<< "${wal_files}"

    # If user did not provide --logs, dump docker logs for correlation
    if [[ -z "${SERVER_LOGS}" ]]; then
        TMP_LOG_FILE=$(mktemp "/tmp/pgvisor-serverlog-XXXXXX.log")
        docker logs "${container}" > "${TMP_LOG_FILE}" 2>&1 || true
        SERVER_LOGS="${TMP_LOG_FILE}"
    fi
}

# ==============================================================================
# dump_from_directory
# Extracts WAL records from offline directory using Docker image or pg_waldump.
#
# Arguments:
#   $1 - Directory path containing WAL segments
#   $2 - Output file for raw waldump
#
# Example usage:
#   dump_from_directory "./wal" "/tmp/waldump.log"
# ==============================================================================
dump_from_directory() {
    local dir="$1"
    local out_file="$2"

    if [[ ! -d "${dir}" ]]; then
        echo "ERROR: Target directory '${dir}' does not exist." >&2
        exit 1
    fi

    local abs_dir
    abs_dir="$(cd "${dir}" && pwd)"

    # Look for WAL segment files (24 hex characters)
    local wal_files
    wal_files=$(find "${abs_dir}" -maxdepth 2 -type f 2>/dev/null | grep -E '/[0-9A-Fa-f]{24}(\.partial)?$' | sort || true)

    if [[ -z "${wal_files}" ]]; then
        echo "WARNING: No 24-character WAL segment files found in '${dir}'" >&2
        return 0
    fi

    # Run containerized pg_waldump to avoid local PostgreSQL version mismatches
    while IFS= read -r wal_path; do
        if [[ -n "${wal_path}" ]]; then
            local fname
            fname="$(basename "${wal_path}")"
            local fdir
            fdir="$(dirname "${wal_path}")"
            docker run --rm --entrypoint pg_waldump -v "${fdir}:/wal:ro" dreamoutbox/pgvisor:latest "/wal/${fname}" >> "${out_file}" 2>/dev/null || true
        fi
    done <<< "${wal_files}"

    # Auto-detect audit logs in sibling directory if not explicitly provided
    if [[ -z "${AUDIT_LOGS}" ]]; then
        local sibling_audit
        sibling_audit=$(find "${abs_dir}/../audit" -maxdepth 1 -name "audit_batch_*.json" 2>/dev/null | sort | tail -n 1 || true)
        if [[ -n "${sibling_audit}" && -f "${sibling_audit}" ]]; then
            AUDIT_LOGS="${sibling_audit}"
        fi
    fi
}

# ==============================================================================
# dump_single_wal
# Extracts WAL records from a single WAL file.
#
# Arguments:
#   $1 - File path to WAL segment
#   $2 - Output file for raw waldump
#
# Example usage:
#   dump_single_wal "./000000010000000000000004" "/tmp/waldump.log"
# ==============================================================================
dump_single_wal() {
    local wal_path="$1"
    local out_file="$2"

    if [[ ! -f "${wal_path}" ]]; then
        echo "ERROR: WAL file '${wal_path}' not found." >&2
        exit 1
    fi

    local abs_path
    abs_path="$(cd "$(dirname "${wal_path}")" && pwd)/$(basename "${wal_path}")"
    local fname
    fname="$(basename "${abs_path}")"
    local fdir
    fdir="$(dirname "${abs_path}")"

    docker run --rm --entrypoint pg_waldump -v "${fdir}:/wal:ro" dreamoutbox/pgvisor:latest "/wal/${fname}" >> "${out_file}" 2>/dev/null || true
}

# Execute WAL dumping based on target
if [[ -n "${CONTAINER}" ]]; then
    dump_from_container "${CONTAINER}" "${TMP_WALDUMP_FILE}"
elif [[ -n "${TARGET_DIR}" ]]; then
    dump_from_directory "${TARGET_DIR}" "${TMP_WALDUMP_FILE}"
elif [[ -n "${TARGET_WAL}" ]]; then
    dump_single_wal "${TARGET_WAL}" "${TMP_WALDUMP_FILE}"
fi

# Build arguments for Python parser
PARSER_ARGS=("--waldump" "${TMP_WALDUMP_FILE}" "--status" "${FILTER_STATUS}")

if [[ -n "${SERVER_LOGS}" && -f "${SERVER_LOGS}" ]]; then
    PARSER_ARGS+=("--logs" "${SERVER_LOGS}")
fi

if [[ -n "${AUDIT_LOGS}" && -f "${AUDIT_LOGS}" ]]; then
    PARSER_ARGS+=("--audit" "${AUDIT_LOGS}")
fi

if [[ -n "${ROW_LIMIT}" ]]; then
    PARSER_ARGS+=("--limit" "${ROW_LIMIT}")
fi

if [[ "${SHOW_WAL_OPS}" == "true" ]]; then
    PARSER_ARGS+=("--wal-ops")
fi

if [[ -n "${MAX_WIDTH}" ]]; then
    PARSER_ARGS+=("--max-width" "${MAX_WIDTH}")
fi

if [[ "${OUTPUT_JSON}" == "true" ]]; then
    PARSER_ARGS+=("--json")
fi

# Execute parser
python3 "${PYTHON_PARSER}" "${PARSER_ARGS[@]}"
