#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Test Docker Compose Profiles Generator Runner
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PYTHON_SCRIPT="${SCRIPT_DIR}/generate_test_composes.py"

if [ ! -f "${PYTHON_SCRIPT}" ]; then
    echo "Error: Generator script not found at ${PYTHON_SCRIPT}"
    exit 1
fi

python3 "${PYTHON_SCRIPT}"
chmod +x "${SCRIPT_DIR}/../composes"/docker-compose.*.yml 2>/dev/null || true
echo "All test compose profiles ready."
