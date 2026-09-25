#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${SCRIPT_DIR}"

TAGS=()
DO_COMPOSE=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        -t|--tag)
            TAGS+=("$2")
            shift 2
            ;;
        --tag=*)
            TAGS+=("${1#*=}")
            shift
            ;;
        --compose)
            DO_COMPOSE=true
            shift
            ;;
        -h|--help)
            echo "Usage: $0 [options] [tag...]"
            echo ""
            echo "Options:"
            echo "  -t, --tag TAG      Image tag to apply (can be specified multiple times)"
            echo "  --compose          Also run 'docker compose build'"
            echo "  -h, --help         Show this help message"
            echo ""
            echo "If no tags are specified, defaults to: pgvisor-test-node:latest pgvisor-test-proxy:latest"
            exit 0
            ;;
        *)
            TAGS+=("$1")
            shift
            ;;
    esac
done

if [ "${#TAGS[@]}" -eq 0 ]; then
    TAGS=("pgvisor-test-node:latest" "pgvisor-test-proxy:latest")
fi

BUILD_ARGS=()
for tag in "${TAGS[@]}"; do
    BUILD_ARGS+=("-t" "${tag}")
done

docker build "${BUILD_ARGS[@]}" "${REPO_ROOT}"

if [ "${DO_COMPOSE}" = true ]; then
    docker compose -f "${REPO_ROOT}/docker-compose.yml" build
fi
