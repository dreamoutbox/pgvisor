#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Developer Backup Metadata Inspection Utility
#
# Inspects backup snapshot metadata (name, label, creation time, last transaction
# time, and last transaction xid) either directly from S3 or offline from a local
# snapshot directory.
#
# All output is strictly clean plain text (no ANSI escape codes).
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/s3-common.sh
source "${SCRIPT_DIR}/scripts/lib/s3-common.sh"

CLUSTER_NAME="${PGVISOR_CLUSTER_NAME:-pgvisor-cluster}"
LOCAL_DIR=""
OUTPUT_JSON=false
TARGET_SNAPSHOT=""

# Ensure jq is installed
if ! command -v jq > /dev/null 2>&1; then
    echo "ERROR: 'jq' is required by dev-inspect-backup.sh but is not installed." >&2
    echo "Please install jq (e.g. 'apt-get install -y jq' or 'brew install jq')." >&2
    exit 1
fi

usage() {
    cat << EOF
Usage: $0 [options] [snapshot-id]

Inspects basebackup metadata from S3 or an offline directory.

Arguments:
  snapshot-id          Optional snapshot ID to inspect (if omitted, lists all backups)

Options:
  -c, --cluster=NAME   Cluster name (default: ${CLUSTER_NAME})
  -d, --dir=DIR        Read from local directory instead of S3
  -b, --bucket=BUCKET  Target S3 bucket name (default: ${S3_BUCKET})
  -e, --endpoint=URL   S3 endpoint URL (default: ${S3_ENDPOINT})
  -j, --json           Output raw JSON instead of aligned table
  -h, --help           Show this help message

Example usage:
  $0
  $0 snap-20261112-015207
  $0 --dir=./debug-s3-bucket-snapshot
  $0 --dir=./debug-s3-bucket-snapshot snap-20261112-015207 --json
EOF
    exit 0
}

# Parse CLI options
while [[ $# -gt 0 ]]; do
    case "$1" in
        -c|--cluster)
            CLUSTER_NAME="$2"
            shift 2
            ;;
        --cluster=*)
            CLUSTER_NAME="${1#*=}"
            shift
            ;;
        -d|--dir)
            LOCAL_DIR="$2"
            shift 2
            ;;
        --dir=*)
            LOCAL_DIR="${1#*=}"
            shift
            ;;
        -b|--bucket)
            S3_BUCKET="$2"
            shift 2
            ;;
        --bucket=*)
            S3_BUCKET="${1#*=}"
            shift
            ;;
        -e|--endpoint)
            S3_ENDPOINT="$2"
            shift 2
            ;;
        --endpoint=*)
            S3_ENDPOINT="${1#*=}"
            shift
            ;;
        -j|--json)
            OUTPUT_JSON=true
            shift
            ;;
        -h|--help)
            usage
            ;;
        -*)
            echo "Unknown option: $1" >&2
            echo "Run '$0 --help' for usage." >&2
            exit 1
            ;;
        *)
            if [ -z "${TARGET_SNAPSHOT}" ]; then
                TARGET_SNAPSHOT="$1"
            else
                echo "Unexpected argument: $1" >&2
                exit 1
            fi
            shift
            ;;
    esac
done

# Collects JSON files from local directory matching snapshot or all basebackups.
#
# Arguments:
#   $1 - Root directory path
#   $2 - Optional target snapshot ID
#
# Output:
#   Array of file paths on stdout.
collect_local_backup_files() {
    local dir="$1"
    local snap="$2"
    local search_dir="${dir}"

    if [ -d "${dir}/clusters/${CLUSTER_NAME}/basebackups" ]; then
        search_dir="${dir}/clusters/${CLUSTER_NAME}/basebackups"
    elif [ -d "${dir}/basebackups" ]; then
        search_dir="${dir}/basebackups"
    fi

    if [ -n "${snap}" ]; then
        local snap_file="${search_dir}/${snap}.json"
        if [ -f "${snap_file}" ]; then
            echo "${snap_file}"
        else
            echo "ERROR: Backup snapshot '${snap}.json' not found in '${search_dir}'" >&2
            return 1
        fi
    else
        find "${search_dir}" -maxdepth 1 -name "*.json" -type f
    fi
}

# Collects JSON content from S3 matching snapshot or all basebackups.
#
# Arguments:
#   $1 - Cluster name
#   $2 - Optional target snapshot ID
#
# Output:
#   Writes downloaded JSON files into temporary directory.
collect_s3_backup_files() {
    local cluster="$1"
    local snap="$2"
    local tmp_dir="$3"
    local base_prefix="clusters/${cluster}/basebackups"

    s3_ensure_bucket_exists "${S3_BUCKET}"

    if [ -n "${snap}" ]; then
        local key="${base_prefix}/${snap}.json"
        local dest="${tmp_dir}/${snap}.json"
        if s3_download_object "${key}" "${dest}" "${S3_BUCKET}"; then
            echo "${dest}"
        else
            echo "ERROR: Backup snapshot '${snap}' not found in S3 bucket '${S3_BUCKET}'" >&2
            return 1
        fi
    else
        local keys
        keys=$(s3_list_all_keys "${S3_BUCKET}" "${base_prefix}/")
        if [ -z "${keys}" ]; then
            return 0
        fi

        while IFS= read -r key; do
            if [[ "${key}" == *.json ]]; then
                local filename
                filename=$(basename "${key}")
                local dest="${tmp_dir}/${filename}"
                s3_download_object "${key}" "${dest}" "${S3_BUCKET}"
                echo "${dest}"
            fi
        done <<< "${keys}"
    fi
}

TMP_FETCH_DIR="/tmp/pgvisor-inspect-$$"
mkdir -p "${TMP_FETCH_DIR}"
cleanup_inspect() {
    rm -rf "${TMP_FETCH_DIR}"
}
trap cleanup_inspect EXIT

# Collect JSON files
FILE_LIST=()
if [ -n "${LOCAL_DIR}" ]; then
    if [ ! -d "${LOCAL_DIR}" ]; then
        echo "ERROR: Directory '${LOCAL_DIR}' does not exist" >&2
        exit 1
    fi
    while IFS= read -r f; do
        if [ -n "${f}" ]; then
            FILE_LIST+=("${f}")
        fi
    done < <(collect_local_backup_files "${LOCAL_DIR}" "${TARGET_SNAPSHOT}")
else
    while IFS= read -r f; do
        if [ -n "${f}" ]; then
            FILE_LIST+=("${f}")
        fi
    done < <(collect_s3_backup_files "${CLUSTER_NAME}" "${TARGET_SNAPSHOT}" "${TMP_FETCH_DIR}")
fi

if [ ${#FILE_LIST[@]} -eq 0 ]; then
    echo "No backup snapshots found."
    exit 0
fi

# Consolidate all JSONs into a single array
COMBINED_JSON=$(jq -s '.' "${FILE_LIST[@]}")

# If --json is requested, output formatted JSON
if [ "${OUTPUT_JSON}" = true ]; then
    if [ -n "${TARGET_SNAPSHOT}" ] && [ ${#FILE_LIST[@]} -eq 1 ]; then
        echo "${COMBINED_JSON}" | jq '.[0]'
    else
        echo "${COMBINED_JSON}" | jq 'sort_by(.created_at) | reverse'
    fi
    exit 0
fi

# Print plain-text aligned table
printf "%-40s  %-20s  %-26s  %-26s  %-12s\n" "SNAPSHOT ID" "LABEL" "CREATED AT" "LAST TX TIME" "LAST TX XID"
printf "%-40s  %-20s  %-26s  %-26s  %-12s\n" "----------------------------------------" "--------------------" "--------------------------" "--------------------------" "------------"

while IFS=$'\t' read -r snap_id label created_at last_tx_time last_tx_xid; do
    printf "%-40s  %-20s  %-26s  %-26s  %-12s\n" \
        "${snap_id}" "${label}" "${created_at}" "${last_tx_time}" "${last_tx_xid}"
done < <(echo "${COMBINED_JSON}" | jq -r '
    sort_by(.created_at) | reverse | .[] | [
        .snapshot_id // "-",
        .label // "-",
        .created_at // "-",
        .last_transaction_time // "-",
        .last_transaction_xid // "-"
    ] | @tsv
')
