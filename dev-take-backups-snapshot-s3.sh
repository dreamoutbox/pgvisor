#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Developer S3 Backups Snapshot Dump Utility
#
# Dumps the entire S3 backups bucket to a local directory for reproducible bugs
# and offline inspection.
#
# All output is strictly clean plain text (no ANSI escape codes).
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/s3-common.sh
source "${SCRIPT_DIR}/scripts/lib/s3-common.sh"

TARGET_DIR="${SNAPSHOT_DIR:-${SCRIPT_DIR}/debug-s3-bucket-snapshot}"
TARGET_BUCKET="${S3_BUCKET}"
TARGET_ENDPOINT="${S3_ENDPOINT}"

usage() {
    cat << EOF
Usage: $0 [options]

Dumps all objects from the S3 backups bucket to a local directory.

Options:
  -d, --dir=DIR        Destination directory (default: ./debug-s3-bucket-snapshot)
  -b, --bucket=BUCKET  Target S3 bucket name (default: ${S3_BUCKET})
  -e, --endpoint=URL   S3 endpoint URL (default: ${S3_ENDPOINT})
  -h, --help           Show this help message

Example usage:
  $0
  $0 --dir=./my-debug-snapshot --bucket=pgvisor-backups
EOF
    exit 0
}

# Parse CLI options
while [[ $# -gt 0 ]]; do
    case "$1" in
        -d|--dir)
            TARGET_DIR="$2"
            shift 2
            ;;
        --dir=*)
            TARGET_DIR="${1#*=}"
            shift
            ;;
        -b|--bucket)
            TARGET_BUCKET="$2"
            shift 2
            ;;
        --bucket=*)
            TARGET_BUCKET="${1#*=}"
            shift
            ;;
        -e|--endpoint)
            TARGET_ENDPOINT="$2"
            shift 2
            ;;
        --endpoint=*)
            TARGET_ENDPOINT="${1#*=}"
            shift
            ;;
        -h|--help)
            usage
            ;;
        *)
            echo "Unknown option: $1" >&2
            echo "Run '$0 --help' for usage." >&2
            exit 1
            ;;
    esac
done

S3_ENDPOINT="${TARGET_ENDPOINT}"
S3_BUCKET="${TARGET_BUCKET}"

echo "Connecting to S3 at ${S3_ENDPOINT} (bucket: ${S3_BUCKET})..."
s3_ensure_bucket_exists "${S3_BUCKET}"

# Collect list of all keys in the bucket
KEYS=$(s3_list_all_keys "${S3_BUCKET}")
if [ -z "${KEYS}" ]; then
    echo "S3 bucket '${S3_BUCKET}' is empty. Nothing to export."
    mkdir -p "${TARGET_DIR}"
    exit 0
fi

KEY_COUNT=$(echo "${KEYS}" | wc -l)
echo "Found ${KEY_COUNT} object(s) in bucket '${S3_BUCKET}'."

# Prepare clean target directory
TMP_DIR="${TARGET_DIR}.tmp.$$"
rm -rf "${TMP_DIR}"
mkdir -p "${TMP_DIR}"

count=0
while IFS= read -r key; do
    if [ -n "${key}" ]; then
        dest_file="${TMP_DIR}/${key}"
        mkdir -p "$(dirname "${dest_file}")"
        s3_download_object "${key}" "${dest_file}" "${S3_BUCKET}"
        count=$((count + 1))
        echo "[${count}/${KEY_COUNT}] Downloaded: ${key}"
    fi
done <<< "${KEYS}"

# Replace old snapshot directory atomically
rm -rf "${TARGET_DIR}"
mv "${TMP_DIR}" "${TARGET_DIR}"

echo "Successfully exported ${count} object(s) from S3 bucket '${S3_BUCKET}' to '${TARGET_DIR}'."
