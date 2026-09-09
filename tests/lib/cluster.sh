#!/usr/bin/env bash
# ==============================================================================
# PgVisor test cluster helpers
#
# Source this file from test scripts:
#   source "${SCRIPT_DIR}/lib/cluster.sh"
#
# Functions provided:
#   cluster_up                 PROJECT COMPOSE_FILE [EXTRA_ARGS...]
#   cluster_down               PROJECT COMPOSE_FILE [EXTRA_ARGS...]
#   start_node                 CONTAINER [PROJECT] [COMPOSE_FILE] [SERVICE]
#   stop_node                  CONTAINER
#   wait_and_remove_minio_init CONTAINER [TIMEOUT_SECS]
#   wait_for_healthy           TIMEOUT_SECONDS CONTAINER [CONTAINER...]
#   wait_for_proxy_ready       DASHBOARD_URL [TIMEOUT_SECS] [AUTH_HEADER...]
# ==============================================================================

# start_node CONTAINER [PROJECT] [COMPOSE_FILE] [SERVICE]
#
# Starts a container directly without docker compose dependency checks, avoiding
# failures when one-shot containers (like minio-init) have already completed and been removed.
# Falls back to docker compose up -d --no-deps if the container was removed.
start_node() {
    local container="$1"
    local project="${2:-}"
    local compose_file="${3:-}"
    local service="${4:-}"

    if docker inspect "${container}" > /dev/null 2>&1; then
        docker start "${container}" > /dev/null
    elif [ -n "${project}" ] && [ -n "${compose_file}" ] && [ -n "${service}" ]; then
        docker compose --progress quiet -p "${project}" -f "${compose_file}" up -d --no-deps "${service}" > /dev/null
    else
        echo "ERROR: Cannot start ${container}: container not found" >&2
        return 1
    fi
}

# stop_node CONTAINER
#
# Stops a container cleanly without docker compose progress output.
stop_node() {
    local container="$1"
    if docker inspect "${container}" > /dev/null 2>&1; then
        docker stop "${container}" > /dev/null
    fi
}

# wait_and_remove_minio_init CONTAINER [TIMEOUT_SECS]
#
# Waits for the one-shot minio-init container to complete its job, asserts that
# it exited cleanly (code 0), and removes the dangling container.
wait_and_remove_minio_init() {
    local container="${1}"
    local timeout_secs="${2:-30}"

    if docker inspect "${container}" > /dev/null 2>&1; then
        local exit_code
        exit_code=$(timeout "${timeout_secs}" docker wait "${container}" 2>/dev/null || echo "timeout")
        if [ "${exit_code}" != "0" ]; then
            echo "ERROR: ${container} failed or timed out with exit code: ${exit_code}" >&2
            echo "--- Container logs for ${container} ---" >&2
            docker logs "${container}" >&2 || true
            return 1
        fi
        docker rm -f "${container}" > /dev/null 2>&1 || true
    fi
    return 0
}

# cluster_up PROJECT COMPOSE_FILE [EXTRA_ARGS...]
#
# Starts the cluster in detached mode with progress suppressed (--progress quiet)
# to prevent docker compose progress noise in test logs. Then waits for and
# cleans up the one-shot minio-init container.
cluster_up() {
    local project="$1"
    local compose_file="$2"
    shift 2

    # --progress quiet must be passed as an option before 'up'
    if ! docker compose --progress quiet -p "${project}" -f "${compose_file}" up -d "$@"; then
        echo "ERROR: Failed to start cluster ${project}" >&2
        return 1
    fi

    # Wait for minio-init to complete and remove it
    wait_and_remove_minio_init "${project}-minio-init" 30
}

# cluster_down PROJECT COMPOSE_FILE [EXTRA_ARGS...]
#
# Tears down the cluster and removes volumes. Swallows all output so the test
# cleanup trap stays clean.
cluster_down() {
    local project="$1"
    local compose_file="$2"
    shift 2 || true
    docker compose -p "${project}" -f "${compose_file}" "$@" down -v --remove-orphans > /dev/null 2>&1 || true
}

# wait_for_healthy TIMEOUT_SECONDS CONTAINER [CONTAINER...]
#
# Polls Docker healthcheck status for every listed container.
# - Fails fast immediately if any container has exited or died.
# - Succeeds once all containers report "healthy" (or "running" if no healthcheck).
# - Fails on timeout with detailed container status.
wait_for_healthy() {
    local timeout_secs="$1"
    shift
    local containers=("$@")

    local deadline=$(( $(date +%s) + timeout_secs ))

    while [ "$(date +%s)" -lt "${deadline}" ]; do
        local all_healthy=true

        for container in "${containers[@]}"; do
            local status
            status=$(docker inspect -f '{{.State.Status}}' "${container}" 2>/dev/null || echo "missing")

            # Fail fast if container exited or crashed
            if [ "${status}" = "exited" ] || [ "${status}" = "dead" ]; then
                local exit_code
                exit_code=$(docker inspect -f '{{.State.ExitCode}}' "${container}" 2>/dev/null || echo "unknown")
                echo "FAIL: Container '${container}' crashed/exited prematurely with code ${exit_code}!" >&2
                echo "--- Logs for ${container} ---" >&2
                docker logs --tail 40 "${container}" >&2 || true
                return 1
            fi

            if [ "${status}" = "missing" ]; then
                all_healthy=false
                break
            fi

            local health
            health=$(docker inspect -f '{{if .State.Health}}{{.State.Health.Status}}{{else}}running{{end}}' "${container}" 2>/dev/null || echo "none")

            if [ "${health}" != "healthy" ] && [ "${health}" != "running" ]; then
                all_healthy=false
                break
            fi
        done

        if [ "${all_healthy}" = "true" ]; then
            return 0
        fi

        sleep 1
    done

    echo "FAIL: Containers did not become healthy within ${timeout_secs}s:" >&2
    for container in "${containers[@]}"; do
        local status health
        status=$(docker inspect -f '{{.State.Status}}' "${container}" 2>/dev/null || echo "missing")
        health=$(docker inspect -f '{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' "${container}" 2>/dev/null || echo "none")
        echo "  - ${container}: status=${status}, health=${health}" >&2
    done
    return 1
}

# wait_for_proxy_ready DASHBOARD_URL [TIMEOUT_SECS] [AUTH_HEADER...]
#
# Waits until the proxy's dashboard responds (HTTP 200 or 401 if unauthenticated).
wait_for_proxy_ready() {
    local dashboard_url="$1"
    local timeout_secs="${2:-30}"
    shift 2 || true
    local auth_header=("$@")

    local deadline=$(( $(date +%s) + timeout_secs ))
    local code="000"
    while [ "$(date +%s)" -lt "${deadline}" ]; do
        code=$(curl -s -o /dev/null -w "%{http_code}" "${auth_header[@]}" "${dashboard_url}/api/status" 2>/dev/null || echo "000")
        if [ "${code}" = "200" ] || [ "${code}" = "401" ]; then
            return 0
        fi
        sleep 1
    done

    echo "FAIL: Proxy dashboard at ${dashboard_url} failed to respond within ${timeout_secs}s (last HTTP code: ${code})" >&2
    return 1
}
