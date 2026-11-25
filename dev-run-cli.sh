#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# Run the native `pgvisor` CLI binary, building it first if missing.
# Usage: ./dev-run-cli.sh [pgvisor args...]
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${SCRIPT_DIR}"
PROFILE="${PGVISOR_CLI_PROFILE:-debug}"

if [[ "${PROFILE}" != "debug" && "${PROFILE}" != "release" ]]; then
    echo "ERROR: Unsupported PGVISOR_CLI_PROFILE '${PROFILE}' (expected 'debug' or 'release')." >&2
    exit 1
fi

CLI_BIN="${REPO_ROOT}/target/${PROFILE}/pgvisor"
if [[ ! -x "${CLI_BIN}" ]]; then
    BUILD_ARGS=()
    if [[ "${PROFILE}" == "release" ]]; then
        BUILD_ARGS+=(--release)
    fi
    "${REPO_ROOT}/dev-build-cli.sh" "${BUILD_ARGS[@]}" >&2
fi

exec "${CLI_BIN}" "$@"
