#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# Build the native `pgvisor` CLI binary (`crates/pgvisor-cli`) for local dev.
# Usage: ./dev-build-cli.sh [--release] [-h|--help]
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${SCRIPT_DIR}"
PROFILE="${PGVISOR_CLI_PROFILE:-debug}"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --release)
            PROFILE="release"
            shift
            ;;
        -h|--help)
            echo "Usage: $0 [--release] [-h|--help]"
            echo ""
            echo "Options:"
            echo "  --release    Build using the Cargo release profile"
            echo "  -h, --help   Show this help message"
            exit 0
            ;;
        *)
            echo "ERROR: Unknown option '$1'" >&2
            exit 1
            ;;
    esac
done

BUILD_ARGS=(--manifest-path "${REPO_ROOT}/Cargo.toml" -p pgvisor-cli)
if [[ "${PROFILE}" == "release" ]]; then
    BUILD_ARGS+=(--release)
elif [[ "${PROFILE}" != "debug" ]]; then
    echo "ERROR: Unsupported PGVISOR_CLI_PROFILE '${PROFILE}' (expected 'debug' or 'release')." >&2
    exit 1
fi

echo "Building pgvisor CLI (${PROFILE})..."
cargo build "${BUILD_ARGS[@]}"

CLI_BIN="${REPO_ROOT}/target/${PROFILE}/pgvisor"
if [[ ! -x "${CLI_BIN}" ]]; then
    echo "ERROR: Expected pgvisor binary not found at ${CLI_BIN}" >&2
    exit 1
fi

echo "Built pgvisor CLI: ${CLI_BIN}"
