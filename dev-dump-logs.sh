#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Dump Node Logs
# Grabs logs of pgvisor-node 1-3 and saves them to the logs/ directory.
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOGS_DIR="${SCRIPT_DIR}/logs"
mkdir -p "${LOGS_DIR}"

WITH_TIMESTAMPS=false
TAIL_LINES=""
DUMP_ALL=false

for arg in "$@"; do
    case "$arg" in
        -t|--timestamps)
            WITH_TIMESTAMPS=true
            ;;
        -a|--all)
            DUMP_ALL=true
            ;;
        --tail=*)
            TAIL_LINES="${arg#*=}"
            ;;
        -h|--help)
            echo "Usage: $0 [options]"
            echo ""
            echo "Dumps logs from pgvisor-node 1-3 into logs/ dir."
            echo ""
            echo "Options:"
            echo "  -t, --timestamps   Include Docker timestamps in output"
            echo "  -a, --all          Also dump pgvisor-proxy and pgvisor-minio logs"
            echo "  --tail=N           Number of lines to show from the end of the logs"
            echo "  -h, --help         Show this help message"
            exit 0
            ;;
        *)
            echo "Unknown option: $arg"
            echo "Run '$0 --help' for usage."
            exit 1
            ;;
    esac
done

DOCKER_OPTS=()
if [ "${WITH_TIMESTAMPS}" = true ]; then
    DOCKER_OPTS+=("-t")
fi
if [ -n "${TAIL_LINES}" ]; then
    DOCKER_OPTS+=("--tail" "${TAIL_LINES}")
fi

echo "Dumping pgvisor node logs into ${LOGS_DIR}..."

for i in 1 2 3; do
    CONTAINER="pgvisor-node${i}"
    LOG_FILE="${LOGS_DIR}/node${i}.log"

    # Match exact container name, or fallback to active container matching node${i}
    if ! docker inspect "${CONTAINER}" &> /dev/null; then
        MATCH=$(docker ps -a --format '{{.Names}}' | grep -E "(^|-)node${i}$" | head -n 1 || true)
        if [ -n "${MATCH}" ]; then
            CONTAINER="${MATCH}"
        fi
    fi

    if docker inspect "${CONTAINER}" &> /dev/null; then
        docker logs "${DOCKER_OPTS[@]}" "${CONTAINER}" 2>&1 | sed -r 's/\x1B\[[0-9;]*[a-zA-Z]//g' > "${LOG_FILE}"
        BYTES=$(wc -c < "${LOG_FILE}" 2>/dev/null || echo 0)
        LINES=$(wc -l < "${LOG_FILE}" 2>/dev/null || echo 0)
        echo "  [node${i}] Dumped ${CONTAINER} -> logs/node${i}.log (${LINES} lines, ${BYTES} bytes)"
    else
        echo "  [node${i}] Container '${CONTAINER}' not found (skipping)"
    fi
done

if [ "${DUMP_ALL}" = true ]; then
    for svc in "pgvisor-proxy" "pgvisor-minio"; do
        CONTAINER="${svc}"
        LOG_FILE="${LOGS_DIR}/${svc#pgvisor-}.log"
        if ! docker inspect "${CONTAINER}" &> /dev/null; then
            MATCH=$(docker ps -a --format '{{.Names}}' | grep -E "(^|-)${svc#pgvisor-}$" | head -n 1 || true)
            if [ -n "${MATCH}" ]; then
                CONTAINER="${MATCH}"
            fi
        fi

        if docker inspect "${CONTAINER}" &> /dev/null; then
            docker logs "${DOCKER_OPTS[@]}" "${CONTAINER}" 2>&1 | sed -r 's/\x1B\[[0-9;]*[a-zA-Z]//g' > "${LOG_FILE}"
            BYTES=$(wc -c < "${LOG_FILE}" 2>/dev/null || echo 0)
            LINES=$(wc -l < "${LOG_FILE}" 2>/dev/null || echo 0)
            echo "  [${svc}] Dumped ${CONTAINER} -> logs/${svc#pgvisor-}.log (${LINES} lines, ${BYTES} bytes)"
        fi
    done
fi

echo "Log dump complete."
