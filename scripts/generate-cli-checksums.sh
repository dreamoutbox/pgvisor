#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# Generate consolidated SHA256SUMS for packaged pgvisor CLI release tarballs.
# Usage: ./scripts/generate-cli-checksums.sh [dist-dir]
# ==============================================================================

DIST_DIR="${1:-dist}"
if [ ! -d "${DIST_DIR}" ]; then
    echo "ERROR: Distribution directory '${DIST_DIR}' does not exist." >&2
    exit 1
fi

cd "${DIST_DIR}"
shopt -s nullglob
ARCHIVES=(pgvisor-*.tar.gz)
if [ ${#ARCHIVES[@]} -eq 0 ]; then
    echo "ERROR: No pgvisor-*.tar.gz archives found in '${DIST_DIR}'." >&2
    exit 1
fi

IFS=$'\n' SORTED=($(printf '%s\n' "${ARCHIVES[@]}" | sort))
unset IFS

if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "${SORTED[@]}" > SHA256SUMS
else
    shasum -a 256 "${SORTED[@]}" > SHA256SUMS
fi

echo "Generated ${DIST_DIR}/SHA256SUMS:"
cat SHA256SUMS
