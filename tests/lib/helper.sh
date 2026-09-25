#!/usr/bin/env bash
# ==============================================================================
# PgVisor test helper library
#
# Source this file from test scripts:
#   source "${SCRIPT_DIR}/lib/helper.sh"
#
# Functions provided:
#   run_node_sql  CONTAINER QUERY
#   run_proxy_sql QUERY [MAX_ATTEMPTS] [PORT] [HOST] [CONTAINER]
#   run_sql       QUERY [MAX_ATTEMPTS] [PORT] [HOST] [CONTAINER]
#   get_sidecar_status CONTAINER
#   json_extract  FIELD
# ==============================================================================

# run_node_sql CONTAINER QUERY
#
# Executes a SQL query directly on a specific container bypassing the proxy.
run_node_sql() {
    local container="$1"
    local query="$2"
    docker exec -i "${container}" psql -h localhost -U postgres -d postgres -t -A -c "${query}" 2>/dev/null || true
}

# run_proxy_sql QUERY [MAX_ATTEMPTS] [PORT] [HOST] [CONTAINER]
#
# Executes a SQL query via the PgVisor proxy with an automatic retry loop (default 5 attempts)
# to absorb replication lag and brief connection transitions.
# Uses local psql if available, falling back to container psql or docker compose exec.
run_proxy_sql() {
    local query="$1"
    local max_attempts="${2:-5}"
    local port="${3:-${PROXY_PORT:-5432}}"
    local host="${4:-${PROXY_HOST:-localhost}}"
    local container="${5:-${PROXY_CONTAINER:-}}"

    local output=""
    local attempt
    for attempt in $(seq 1 "${max_attempts}"); do
        if command -v psql &> /dev/null; then
            if output=$(PGPASSWORD="" PGCONNECT_TIMEOUT=5 timeout 15 psql -h "${host}" -p "${port}" -U postgres -d postgres -t -A -c "${query}" 2>&1); then
                echo "${output}"
                return 0
            fi
        elif [ -n "${container}" ]; then
            if output=$(timeout 15 docker exec -i "${container}" psql -h localhost -p 5432 -U postgres -d postgres -t -A -c "${query}" 2>&1); then
                echo "${output}"
                return 0
            fi
        elif [ -n "${PROJECT_NAME:-}" ] && [ -n "${COMPOSE_FILE:-}" ]; then
            if output=$(docker compose -p "${PROJECT_NAME}" -f "${COMPOSE_FILE}" exec -T pgvisor-proxy psql -h localhost -p 5432 -U postgres -d postgres -t -A -c "${query}" 2>&1); then
                echo "${output}"
                return 0
            fi
        fi
        sleep 1
    done
    echo "${output}"
    return 1
}

# run_sql QUERY [MAX_ATTEMPTS] [PORT] [HOST] [CONTAINER]
#
# Alias for run_proxy_sql to provide uniform naming across test suites.
run_sql() {
    run_proxy_sql "$@"
}

# get_sidecar_status CONTAINER
#
# Queries the sidecar control status endpoint on localhost:8080.
get_sidecar_status() {
    local container="$1"
    docker exec -i "${container}" curl -s http://localhost:8080/control/status 2>/dev/null || true
}

# json_extract FIELD
#
# Extracts a top-level field from JSON received via stdin using jq, python3, or grep fallback.
json_extract() {
    local field="$1"
    if command -v jq &> /dev/null; then
        jq -r ".${field} // empty"
    elif command -v python3 &> /dev/null; then
        python3 -c "import sys, json; data = json.load(sys.stdin); print(data.get('${field}', ''))" 2>/dev/null || true
    else
        grep -o "\"${field}\":\"[^\"]*\"" | cut -d':' -f2 | tr -d '"'
    fi
}
