#!/usr/bin/env bash
# ==============================================================================
# PgVisor: S3 Common Client Library
#
# Provides reusable S3 object storage operations using curl AWS SigV4.
# Compatible with AWS S3, MinIO, RustFS, and SeaweedFS.
# All output is strictly clean plain text (no ANSI escape codes).
# ==============================================================================

S3_COMMON_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PARSE_S3_XML="${S3_COMMON_DIR}/parse-s3-xml.py"

# Default configuration with environment variable overrides
S3_ENDPOINT="${S3_ENDPOINT:-http://127.0.0.1:9000}"
S3_BUCKET="${S3_BUCKET:-pgvisor-backups}"
S3_ACCESS_KEY="${S3_ACCESS_KEY:-${RUSTFS_ACCESS_KEY:-minioadmin}}"
S3_SECRET_KEY="${S3_SECRET_KEY:-${RUSTFS_SECRET_KEY:-minioadmin}}"
S3_REGION="${S3_REGION:-us-east-1}"

EMPTY_PAYLOAD_SHA256="e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"

# Ensures the S3 bucket exists, creating it if necessary.
#
# Arguments:
#   $1 - Optional bucket name (defaults to $S3_BUCKET)
#
# Example usage:
#   s3_ensure_bucket_exists "pgvisor-backups"
s3_ensure_bucket_exists() {
    local target_bucket="${1:-${S3_BUCKET}}"
    local target_url="${S3_ENDPOINT}/${target_bucket}"

    # Check if bucket exists
    if curl -s -f -I \
        --aws-sigv4 "aws:amz:${S3_REGION}:s3" \
        --user "${S3_ACCESS_KEY}:${S3_SECRET_KEY}" \
        -H "x-amz-content-sha256: ${EMPTY_PAYLOAD_SHA256}" \
        "${target_url}" > /dev/null 2>&1; then
        return 0
    fi

    # Create bucket
    if ! curl -s -f -X PUT \
        --aws-sigv4 "aws:amz:${S3_REGION}:s3" \
        --user "${S3_ACCESS_KEY}:${S3_SECRET_KEY}" \
        -H "x-amz-content-sha256: ${EMPTY_PAYLOAD_SHA256}" \
        "${target_url}" > /dev/null 2>&1; then
        # Retry with HEAD in case created concurrently
        curl -s -f -I \
            --aws-sigv4 "aws:amz:${S3_REGION}:s3" \
            --user "${S3_ACCESS_KEY}:${S3_SECRET_KEY}" \
            -H "x-amz-content-sha256: ${EMPTY_PAYLOAD_SHA256}" \
            "${target_url}" > /dev/null 2>&1
    fi
}

# Lists all object keys in the specified bucket (handling pagination).
#
# Arguments:
#   $1 - Optional bucket name (defaults to $S3_BUCKET)
#   $2 - Optional prefix filter
#
# Output:
#   Outputs object keys line by line to stdout.
#
# Example usage:
#   s3_list_all_keys "pgvisor-backups" "clusters/pgvisor-cluster/"
s3_list_all_keys() {
    local target_bucket="${1:-${S3_BUCKET}}"
    local prefix="${2:-}"
    local continuation_token=""

    while true; do
        local query="list-type=2"
        if [ -n "${prefix}" ]; then
            query="${query}&prefix=${prefix}"
        fi
        if [ -n "${continuation_token}" ]; then
            query="${query}&continuation-token=${continuation_token}"
        fi

        local xml_resp
        xml_resp=$(curl -s -f \
            --aws-sigv4 "aws:amz:${S3_REGION}:s3" \
            --user "${S3_ACCESS_KEY}:${S3_SECRET_KEY}" \
            -H "x-amz-content-sha256: ${EMPTY_PAYLOAD_SHA256}" \
            "${S3_ENDPOINT}/${target_bucket}?${query}") || break

        echo "${xml_resp}" | "${PARSE_S3_XML}" --keys

        local is_truncated
        is_truncated=$(echo "${xml_resp}" | "${PARSE_S3_XML}" --is-truncated)
        if [ "${is_truncated}" != "true" ]; then
            break
        fi

        continuation_token=$(echo "${xml_resp}" | "${PARSE_S3_XML}" --next-token)
        if [ -z "${continuation_token}" ]; then
            break
        fi
    done
}

# Downloads a single object from S3 to a local file destination.
#
# Arguments:
#   $1 - S3 object key
#   $2 - Local destination file path
#   $3 - Optional bucket name (defaults to $S3_BUCKET)
#
# Example usage:
#   s3_download_object "clusters/cluster1/meta.json" "/tmp/meta.json"
s3_download_object() {
    local key="$1"
    local local_path="$2"
    local target_bucket="${3:-${S3_BUCKET}}"

    mkdir -p "$(dirname "${local_path}")"
    curl -s -f \
        --aws-sigv4 "aws:amz:${S3_REGION}:s3" \
        --user "${S3_ACCESS_KEY}:${S3_SECRET_KEY}" \
        -H "x-amz-content-sha256: ${EMPTY_PAYLOAD_SHA256}" \
        "${S3_ENDPOINT}/${target_bucket}/${key}" \
        -o "${local_path}"
}

# Uploads a local file to S3 at the given object key.
#
# Arguments:
#   $1 - Local file path to upload
#   $2 - Target S3 object key
#   $3 - Optional bucket name (defaults to $S3_BUCKET)
#
# Example usage:
#   s3_upload_object "/tmp/meta.json" "clusters/cluster1/meta.json"
s3_upload_object() {
    local local_path="$1"
    local key="$2"
    local target_bucket="${3:-${S3_BUCKET}}"

    if [ ! -f "${local_path}" ]; then
        echo "ERROR: Local file '${local_path}' does not exist" >&2
        return 1
    fi

    local payload_sha
    payload_sha=$(sha256sum "${local_path}" | awk '{print $1}')

    curl -s -f -X PUT \
        --aws-sigv4 "aws:amz:${S3_REGION}:s3" \
        --user "${S3_ACCESS_KEY}:${S3_SECRET_KEY}" \
        -H "x-amz-content-sha256: ${payload_sha}" \
        --data-binary "@${local_path}" \
        "${S3_ENDPOINT}/${target_bucket}/${key}" > /dev/null
}

# Deletes an object key from the specified bucket.
#
# Arguments:
#   $1 - S3 object key
#   $2 - Optional bucket name (defaults to $S3_BUCKET)
#
# Example usage:
#   s3_delete_object "clusters/cluster1/temp.bin"
s3_delete_object() {
    local key="$1"
    local target_bucket="${2:-${S3_BUCKET}}"

    curl -s -f -X DELETE \
        --aws-sigv4 "aws:amz:${S3_REGION}:s3" \
        --user "${S3_ACCESS_KEY}:${S3_SECRET_KEY}" \
        -H "x-amz-content-sha256: ${EMPTY_PAYLOAD_SHA256}" \
        "${S3_ENDPOINT}/${target_bucket}/${key}" > /dev/null
}

# Empties all objects from the specified bucket.
#
# Arguments:
#   $1 - Optional bucket name (defaults to $S3_BUCKET)
#
# Example usage:
#   s3_wipe_bucket "pgvisor-backups"
s3_wipe_bucket() {
    local target_bucket="${1:-${S3_BUCKET}}"
    local keys
    keys=$(s3_list_all_keys "${target_bucket}")

    if [ -z "${keys}" ]; then
        return 0
    fi

    while IFS= read -r key; do
        if [ -n "${key}" ]; then
            s3_delete_object "${key}" "${target_bucket}"
        fi
    done <<< "${keys}"
}

