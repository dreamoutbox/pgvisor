#!/usr/bin/env bash
# ==============================================================================
# PgVisor test helper library
#
# Source this file from test scripts:
#   source "${SCRIPT_DIR}/lib/helper.sh"
#
# Functions provided:
#   run_node_sql CONTAINER QUERY
#   get_sidecar_status CONTAINER
#   json_extract FIELD
# ==============================================================================

# run_node_sql CONTAINER QUERY
#
# Executes a SQL query directly on a specific container bypassing the proxy.
run_node_sql() {
    local container="$1"
    local query="$2"
    docker exec -i "${container}" psql -U postgres -d postgres -t -A -c "${query}" 2>/dev/null || true
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
