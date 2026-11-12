#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Developer S3 Snapshot Export/Import Verification Test
#
# Tests dev-take-backups-snapshot-s3.sh and dev-import-backups-snapshot-s3.sh
# against an S3 bucket with sample files and verification.
#
# All output is strictly clean plain text (no ANSI escape codes).
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
# shellcheck source=scripts/lib/s3-common.sh
source "${REPO_ROOT}/scripts/lib/s3-common.sh"

TEST_BUCKET="pgvisor-test-snap-$$"
TEST_DIR="/tmp/pgvisor-test-snap-dir-$$"

cleanup() {
    echo "Cleaning up test bucket and temporary directory..."
    s3_wipe_bucket "${TEST_BUCKET}" 2>/dev/null || true
    rm -rf "${TEST_DIR}"
}
trap cleanup EXIT

echo "========================================================="
echo "  PgVisor S3 Snapshot Export/Import Verification Test    "
echo "  Bucket: ${TEST_BUCKET}                                 "
echo "========================================================="

# [1/4] Ensure test bucket exists and populate sample files
echo "[1/4] Ensuring test bucket exists and uploading sample objects..."
s3_ensure_bucket_exists "${TEST_BUCKET}"

SAMPLE_DIR="/tmp/pgvisor-sample-$$"
mkdir -p "${SAMPLE_DIR}/clusters/test-cluster/basebackups"
mkdir -p "${SAMPLE_DIR}/clusters/test-cluster/wal"

echo '{"snapshot_id":"snap-test-01","last_transaction_time":"2026-11-12T00:00:00Z","last_transaction_xid":"500"}' > "${SAMPLE_DIR}/clusters/test-cluster/basebackups/snap-test-01.json"
echo "archive-bytes-01" > "${SAMPLE_DIR}/clusters/test-cluster/basebackups/snap-test-01.tar.gz"
echo "wal-content-01" > "${SAMPLE_DIR}/clusters/test-cluster/wal/000000010000000000000001"

s3_upload_object "${SAMPLE_DIR}/clusters/test-cluster/basebackups/snap-test-01.json" "clusters/test-cluster/basebackups/snap-test-01.json" "${TEST_BUCKET}"
s3_upload_object "${SAMPLE_DIR}/clusters/test-cluster/basebackups/snap-test-01.tar.gz" "clusters/test-cluster/basebackups/snap-test-01.tar.gz" "${TEST_BUCKET}"
s3_upload_object "${SAMPLE_DIR}/clusters/test-cluster/wal/000000010000000000000001" "clusters/test-cluster/wal/000000010000000000000001" "${TEST_BUCKET}"
rm -rf "${SAMPLE_DIR}"

echo "✓ Sample objects uploaded to S3 bucket '${TEST_BUCKET}'."

# [2/4] Run dev-take-backups-snapshot-s3.sh
echo ""
echo "[2/4] Testing dev-take-backups-snapshot-s3.sh..."
"${REPO_ROOT}/dev-take-backups-snapshot-s3.sh" --bucket="${TEST_BUCKET}" --dir="${TEST_DIR}"

if [ ! -f "${TEST_DIR}/clusters/test-cluster/basebackups/snap-test-01.json" ]; then
    echo "FAILED: snap-test-01.json not found in exported directory!"
    exit 1
fi
if [ ! -f "${TEST_DIR}/clusters/test-cluster/basebackups/snap-test-01.tar.gz" ]; then
    echo "FAILED: snap-test-01.tar.gz not found in exported directory!"
    exit 1
fi
if [ ! -f "${TEST_DIR}/clusters/test-cluster/wal/000000010000000000000001" ]; then
    echo "FAILED: wal file not found in exported directory!"
    exit 1
fi

CONTENT=$(cat "${TEST_DIR}/clusters/test-cluster/basebackups/snap-test-01.json")
if [[ "${CONTENT}" != *"snap-test-01"* ]]; then
    echo "FAILED: Content mismatch in exported snap-test-01.json!"
    exit 1
fi
echo "✓ Verified exported snapshot files and content match."

# [3/4] Wipe bucket to simulate fresh state
echo ""
echo "[3/4] Wiping S3 bucket '${TEST_BUCKET}'..."
s3_wipe_bucket "${TEST_BUCKET}"
REMAINING_KEYS=$(s3_list_all_keys "${TEST_BUCKET}")
if [ -n "${REMAINING_KEYS}" ]; then
    echo "FAILED: S3 bucket '${TEST_BUCKET}' was not empty after wipe!"
    exit 1
fi
echo "✓ Bucket successfully cleared."

# [4/4] Run dev-import-backups-snapshot-s3.sh
echo ""
echo "[4/4] Testing dev-import-backups-snapshot-s3.sh..."
"${REPO_ROOT}/dev-import-backups-snapshot-s3.sh" --bucket="${TEST_BUCKET}" --dir="${TEST_DIR}"

RESTORED_KEYS=$(s3_list_all_keys "${TEST_BUCKET}")
echo "Restored keys:"
echo "${RESTORED_KEYS}"

if ! echo "${RESTORED_KEYS}" | grep -q "clusters/test-cluster/basebackups/snap-test-01.json"; then
    echo "FAILED: Restored bucket missing snap-test-01.json!"
    exit 1
fi
if ! echo "${RESTORED_KEYS}" | grep -q "clusters/test-cluster/wal/000000010000000000000001"; then
    echo "FAILED: Restored bucket missing wal segment!"
    exit 1
fi

echo "✓ S3 Snapshot Export/Import verification passed successfully."
