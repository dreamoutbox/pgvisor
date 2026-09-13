#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor Root Bootstrap Entrypoint
# Delegates execution to examples/setup.sh
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "${SCRIPT_DIR}/examples/setup.sh" "$@"
