#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Developer S3 Backups Snapshot Import Utility
#
# Restores S3 bucket contents from a local snapshot directory (e.g. debug-s3-bucket-snapshot)
# for reproducing bugs and verifying restore behavior.
#
# All output is strictly clean plain text (no ANSI escape codes).
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/s3-common.sh
source "${SCRIPT_DIR}/scripts/lib/s3-common.sh"

SRC_DIR="${SNAPSHOT_DIR:-${SCRIPT_DIR}/debug-s3-bucket-snapshot}"
TARGET_BUCKET="${S3_BUCKET}"
TARGET_ENDPOINT="${S3_ENDPOINT}"
DO_CLEAN=false

usage() {
    cat << EOF
Usage: $0 [options]

Uploads all files from a local snapshot directory into the S3 backups bucket.

Options:
  -d, --dir=DIR        Source directory to import from (default: ./debug-s3-bucket-snapshot)
  -b, --bucket=BUCKET  Target S3 bucket name (default: ${S3_BUCKET})
  -e, --endpoint=URL   S3 endpoint URL (default: ${S3_ENDPOINT})
  -c, --clean          Wipe target S3 bucket before importing
  -h, --help           Show this help message

Example usage:
  $0
  $0 --clean
  $0 --dir=./my-debug-snapshot --bucket=pgvisor-backups --clean
EOF
    exit 0
}

# Parse CLI options
while [[ $# -gt 0 ]]; do
    case "$1" in
        -d|--dir)
            SRC_DIR="$2"
            shift 2
            ;;
        --dir=*)
            SRC_DIR="${1#*=}"
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
        -c|--clean)
            DO_CLEAN=true
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

if [ ! -d "${SRC_DIR}" ]; then
    echo "ERROR: Snapshot directory '${SRC_DIR}' does not exist." >&2
    exit 1
fi

echo "Connecting to S3 at ${S3_ENDPOINT} (bucket: ${S3_BUCKET})..."
s3_ensure_bucket_exists "${S3_BUCKET}"

if [ "${DO_CLEAN}" = true ]; then
    echo "Cleaning existing objects in S3 bucket '${S3_BUCKET}'..."
    s3_wipe_bucket "${S3_BUCKET}"
    echo "Bucket '${S3_BUCKET}' is now clean."
fi

# Find all files in SRC_DIR
mapfile -t FILES < <(find "${SRC_DIR}" -type f)
TOTAL_FILES=${#FILES[@]}

if [ "${TOTAL_FILES}" -eq 0 ]; then
    echo "Warning: Snapshot directory '${SRC_DIR}' contains no files. Nothing to import."
    exit 0
fi

echo "Importing ${TOTAL_FILES} file(s) from '${SRC_DIR}' into bucket '${S3_BUCKET}'..."

count=0
for local_file in "${FILES[@]}"; do
    # Derive object key relative to SRC_DIR
    rel_key="${local_file#"${SRC_DIR}"/}"
    s3_upload_object "${local_file}" "${rel_key}" "${S3_BUCKET}"
    count=$((count + 1))
    echo "[${count}/${TOTAL_FILES}] Uploaded: ${rel_key}"
done

echo "Successfully imported ${count} object(s) into S3 bucket '${S3_BUCKET}'."
