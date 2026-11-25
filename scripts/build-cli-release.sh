#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# Build and package a release archive + SHA-256 checksum for pgvisor-cli.
# Usage: ./scripts/build-cli-release.sh <target-triple> [version]
# ==============================================================================

if [ $# -lt 1 ]; then
    echo "Usage: $0 <target-triple> [version]" >&2
    exit 1
fi

TARGET="$1"
RAW_VERSION="${2:-${GITHUB_REF_NAME:-dev}}"
VERSION="${RAW_VERSION#v}"
DIST_DIR="dist"
ARCHIVE_NAME="pgvisor-${VERSION}-${TARGET}.tar.gz"

echo "Building pgvisor-cli v${VERSION} for target ${TARGET}..."
cargo build --release -p pgvisor-cli --target "${TARGET}"

BIN_PATH="target/${TARGET}/release/pgvisor"
if [ ! -x "${BIN_PATH}" ]; then
    echo "ERROR: Expected executable binary not found at ${BIN_PATH}" >&2
    exit 1
fi

mkdir -p "${DIST_DIR}"
STAGE_DIR="$(mktemp -d)"
trap 'rm -rf "${STAGE_DIR}"' EXIT

cp "${BIN_PATH}" "${STAGE_DIR}/pgvisor"
chmod 755 "${STAGE_DIR}/pgvisor"

tar -C "${STAGE_DIR}" -czf "${DIST_DIR}/${ARCHIVE_NAME}" pgvisor

if command -v sha256sum >/dev/null 2>&1; then
    (cd "${DIST_DIR}" && sha256sum "${ARCHIVE_NAME}" > "${ARCHIVE_NAME}.sha256")
else
    (cd "${DIST_DIR}" && shasum -a 256 "${ARCHIVE_NAME}" > "${ARCHIVE_NAME}.sha256")
fi

echo "Created ${DIST_DIR}/${ARCHIVE_NAME} and ${DIST_DIR}/${ARCHIVE_NAME}.sha256"
