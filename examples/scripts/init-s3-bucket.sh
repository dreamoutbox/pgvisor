#!/usr/bin/env sh
set -eu

# ==============================================================================
# S3 / RustFS Bucket Initializer
#
# Idempotently creates the target S3 bucket using native curl AWS SigV4.
# Retries until the endpoint is healthy and the bucket is verified.
# ==============================================================================

ENDPOINT="${S3_ENDPOINT:-http://minio:9000}"
BUCKET="${S3_BUCKET:-pgvisor-backups}"
ACCESS_KEY="${S3_ACCESS_KEY:-${RUSTFS_ACCESS_KEY:-minioadmin}}"
SECRET_KEY="${S3_SECRET_KEY:-${RUSTFS_SECRET_KEY:-minioadmin}}"
REGION="${S3_REGION:-us-east-1}"
MAX_ATTEMPTS="${MAX_ATTEMPTS:-30}"

EMPTY_PAYLOAD_SHA256="e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
TARGET_URL="${ENDPOINT}/${BUCKET}"

echo "Waiting for S3 endpoint ${ENDPOINT} and ensuring bucket '${BUCKET}' exists..."

attempt=1
while [ "${attempt}" -le "${MAX_ATTEMPTS}" ]; do
    # Try S3 PUT bucket with AWS SigV4
    if curl -s -f -X PUT \
        --aws-sigv4 "aws:amz:${REGION}:s3" \
        --user "${ACCESS_KEY}:${SECRET_KEY}" \
        -H "x-amz-content-sha256: ${EMPTY_PAYLOAD_SHA256}" \
        "${TARGET_URL}" > /dev/null 2>&1; then
        echo "Successfully initialized S3 bucket '${BUCKET}' at ${ENDPOINT}"
        exit 0
    fi

    # Check if bucket already exists (HTTP 200 from HEAD)
    if curl -s -f -I \
        --aws-sigv4 "aws:amz:${REGION}:s3" \
        --user "${ACCESS_KEY}:${SECRET_KEY}" \
        -H "x-amz-content-sha256: ${EMPTY_PAYLOAD_SHA256}" \
        "${TARGET_URL}" > /dev/null 2>&1; then
        echo "S3 bucket '${BUCKET}' already exists at ${ENDPOINT}"
        exit 0
    fi

    echo "Attempt ${attempt}/${MAX_ATTEMPTS}: S3 bucket not ready yet, retrying in 1s..."
    attempt=$((attempt + 1))
    sleep 1
done

echo "ERROR: Timed out waiting for S3 bucket '${BUCKET}' after ${MAX_ATTEMPTS} attempts." >&2
exit 1
