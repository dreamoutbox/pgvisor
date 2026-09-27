#!/usr/bin/env bash
# ==============================================================================
# PgVisor Docker Swarm DinD Test Helper Library
#
# Provides isolated Docker-in-Docker (DinD) Swarm orchestration helpers:
#   swarm_up                        DIND_NAME PROXY_PORT DASHBOARD_PORT [MINIO_PORT]
#   swarm_down                      [DIND_NAME]
#   swarm_docker                    [ARGS...]
#   swarm_stack_deploy              STACK_NAME STACK_FILE
#   swarm_stack_rm                  STACK_NAME
#   swarm_init_s3_bucket            STACK_NAME [NETWORK_NAME]
#   swarm_wait_for_healthy          TIMEOUT_SECS SERVICE_NAME [SERVICE_NAME...]
#   swarm_wait_for_proxy_ready      DASHBOARD_URL [TIMEOUT_SECS] [AUTH_HEADER...]
#   swarm_run_proxy_sql             QUERY [MAX_ATTEMPTS] [PORT] [HOST]
#   swarm_run_node_sql              SERVICE_NAME QUERY
#   swarm_get_sidecar_status        SERVICE_NAME
#   swarm_scale_service             SERVICE_NAME REPLICAS
#   swarm_cleanup                   [DIND_NAME]
# ==============================================================================

SCRIPT_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SWARM_REPO_ROOT="$(cd "${SCRIPT_LIB_DIR}/../.." && pwd)"

# shellcheck source=tests/lib/helper.sh
source "${SCRIPT_LIB_DIR}/helper.sh"

SWARM_DIND_CONTAINER=""
SWARM_STACK_NAME=""
SWARM_PROXY_PORT=5432
SWARM_DASHBOARD_PORT=8080

# ensure_swarm_image_cache
#
# Caches required images to an archive on the host so DinD loads are fast and
# independent across concurrent test executions. Re-exports if the host image ID changes.
ensure_swarm_image_cache() {
    local cache_tar="/tmp/pgvisor-swarm-images.tar"
    local cache_id_file="/tmp/pgvisor-swarm-images.id"

    local current_node_id
    current_node_id=$(docker image inspect -f '{{.Id}}' pgvisor-test-node:latest 2>/dev/null || true)
    local current_minio_id
    current_minio_id=$(docker image inspect -f '{{.Id}}' rustfs/rustfs:latest 2>/dev/null || true)
    local current_combined="${current_node_id}_${current_minio_id}"

    if [ -z "${current_node_id}" ]; then
        echo "Building PgVisor test images first..."
        docker build -t pgvisor-test-node:latest -t pgvisor-test-proxy:latest "${SWARM_REPO_ROOT}" >/dev/null
        current_node_id=$(docker image inspect -f '{{.Id}}' pgvisor-test-node:latest)
        current_combined="${current_node_id}_${current_minio_id}"
    fi

    local cached_id=""
    if [ -f "${cache_id_file}" ]; then
        cached_id=$(cat "${cache_id_file}")
    fi

    if [ ! -f "${cache_tar}" ] || [ "${current_combined}" != "${cached_id}" ]; then
        echo "Exporting Docker images for DinD Swarm testing..."
        docker save pgvisor-test-node:latest rustfs/rustfs:latest -o "${cache_tar}"
        echo "${current_combined}" > "${cache_id_file}"
    fi
}

# swarm_up DIND_NAME PROXY_PORT DASHBOARD_PORT [MINIO_PORT]
#
# Starts an isolated DinD daemon container, initializes Docker Swarm,
# and loads cached test images into the DinD daemon.
swarm_up() {
    local dind_name="$1"
    local proxy_port="$2"
    local dashboard_port="$3"
    local minio_port="${4:-}"

    SWARM_DIND_CONTAINER="${dind_name}"
    SWARM_PROXY_PORT="${proxy_port}"
    SWARM_DASHBOARD_PORT="${dashboard_port}"

    ensure_swarm_image_cache

    swarm_down "${dind_name}"

    local port_args=(
        -p "${proxy_port}:5432"
        -p "${dashboard_port}:8080"
    )
    if [ -n "${minio_port}" ]; then
        port_args+=(-p "${minio_port}:9000")
    fi

    echo "Launching Docker-in-Docker container: ${dind_name}..."
    docker run -d --privileged \
        --name "${dind_name}" \
        "${port_args[@]}" \
        -v "${SWARM_REPO_ROOT}:/workspace:ro" \
        -v "/tmp/pgvisor-swarm-images.tar:/images.tar:ro" \
        docker:dind >/dev/null

    echo "Waiting for DinD Docker daemon to become responsive..."
    local retries=30
    while [ $retries -gt 0 ]; do
        if docker exec "${dind_name}" docker info >/dev/null 2>&1; then
            break
        fi
        retries=$((retries - 1))
        sleep 1
    done

    if [ $retries -eq 0 ]; then
        echo "ERROR: Timed out waiting for Docker daemon inside ${dind_name}" >&2
        return 1
    fi

    echo "Initializing Docker Swarm inside DinD..."
    docker exec "${dind_name}" docker swarm init >/dev/null

    echo "Loading cached images into DinD..."
    docker exec "${dind_name}" docker load -i /images.tar >/dev/null
    docker exec "${dind_name}" docker tag pgvisor-test-node:latest pgvisor-test-proxy:latest >/dev/null 2>&1 || true
    echo "+ DinD Swarm node ready."
}

# swarm_down [DIND_NAME]
#
# Stops and removes the DinD container cleanly.
swarm_down() {
    local dind_name="${1:-${SWARM_DIND_CONTAINER}}"
    if [ -n "${dind_name}" ] && docker inspect "${dind_name}" >/dev/null 2>&1; then
        docker rm -f "${dind_name}" >/dev/null 2>&1 || true
    fi
}

# swarm_docker [ARGS...]
#
# Runs a docker CLI command against the active DinD Docker daemon.
swarm_docker() {
    docker exec "${SWARM_DIND_CONTAINER}" docker "$@"
}

# swarm_stack_deploy STACK_NAME STACK_FILE
#
# Deploys a stack file inside the active DinD Swarm daemon.
swarm_stack_deploy() {
    local stack_name="$1"
    local stack_file="$2"
    SWARM_STACK_NAME="${stack_name}"

    local abs_stack_file
    if [[ "${stack_file}" = /* ]]; then
        abs_stack_file="${stack_file}"
    else
        abs_stack_file="$(cd "$(dirname "${stack_file}")" && pwd)/$(basename "${stack_file}")"
    fi

    local internal_stack_file="${abs_stack_file}"
    if [[ "${abs_stack_file}" == "${SWARM_REPO_ROOT}"* ]]; then
        internal_stack_file="/workspace${abs_stack_file#${SWARM_REPO_ROOT}}"
    fi

    echo "Deploying Swarm stack '${stack_name}' from ${internal_stack_file}..."
    docker exec "${SWARM_DIND_CONTAINER}" docker stack deploy -c "${internal_stack_file}" "${stack_name}" >/dev/null
}

# swarm_stack_rm STACK_NAME
#
# Removes a stack inside DinD and waits for services to terminate.
swarm_stack_rm() {
    local stack_name="${1:-${SWARM_STACK_NAME}}"
    if [ -n "${stack_name}" ]; then
        docker exec "${SWARM_DIND_CONTAINER}" docker stack rm "${stack_name}" >/dev/null 2>&1 || true
    fi
}

# swarm_init_s3_bucket STACK_NAME [NETWORK_NAME]
#
# Runs the S3 bucket initialization script attached to the Swarm overlay network.
swarm_init_s3_bucket() {
    local stack_name="${1:-${SWARM_STACK_NAME}}"
    local network_name="${2:-${stack_name}_pgvisor-net}"

    echo "Initializing S3 backup bucket in Swarm network ${network_name}..."
    docker exec "${SWARM_DIND_CONTAINER}" docker run --rm \
        --network "${network_name}" \
        -e S3_ENDPOINT="http://minio:9000" \
        -e S3_BUCKET="pgvisor-backups" \
        -e S3_ACCESS_KEY="minioadmin" \
        -e S3_SECRET_KEY="minioadmin" \
        -v /workspace/scripts:/scripts:ro \
        --entrypoint /scripts/init-s3-bucket.sh \
        rustfs/rustfs:latest >/dev/null
}

# swarm_find_container SERVICE_NAME
#
# Finds the active task container ID for a service in DinD.
swarm_find_container() {
    local service_name="$1"
    local cid
    cid=$(docker exec "${SWARM_DIND_CONTAINER}" docker ps -q --filter "name=${SWARM_STACK_NAME}_${service_name}" | head -n 1)
    if [ -z "${cid}" ] && [[ "${service_name}" != *"pgvisor-"* ]]; then
        cid=$(docker exec "${SWARM_DIND_CONTAINER}" docker ps -q --filter "name=${SWARM_STACK_NAME}_pgvisor-${service_name}" | head -n 1)
    fi
    echo "${cid}"
}

# swarm_wait_for_healthy TIMEOUT_SECS SERVICE_NAME [SERVICE_NAME...]
#
# Waits for Swarm task containers to report healthy.
swarm_wait_for_healthy() {
    local timeout="$1"
    shift
    local services=("$@")
    local elapsed=0

    echo "Waiting up to ${timeout}s for Swarm services: ${services[*]}..."
    for service in "${services[@]}"; do
        local svc_ready=false

        while [ "${elapsed}" -lt "${timeout}" ]; do
            # Find running task container id for this service
            local cid
            cid=$(swarm_find_container "${service}")

            if [ -n "${cid}" ]; then
                local is_running
                is_running=$(docker exec "${SWARM_DIND_CONTAINER}" docker inspect --format '{{.State.Status}}' "${cid}" 2>/dev/null || echo "")

                if [ "${is_running}" = "running" ]; then
                    local health_status
                    health_status=$(docker exec "${SWARM_DIND_CONTAINER}" docker inspect --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' "${cid}" 2>/dev/null || echo "")

                    if [ "${health_status}" = "healthy" ] || [ "${health_status}" = "none" ]; then
                        svc_ready=true
                        break
                    fi
                elif [ "${is_running}" = "exited" ] || [ "${is_running}" = "dead" ]; then
                    echo "ERROR: Container ${cid} for ${service} crashed with status: ${is_running}" >&2
                    docker exec "${SWARM_DIND_CONTAINER}" docker logs "${cid}" >&2 || true
                    return 1
                fi
            fi

            sleep 2
            elapsed=$((elapsed + 2))
        done

        if [ "${svc_ready}" != "true" ]; then
            echo "ERROR: Service ${service} failed to become healthy within ${timeout}s" >&2
            echo "--- Tasks for ${service} ---" >&2
            docker exec "${SWARM_DIND_CONTAINER}" docker service ps "${SWARM_STACK_NAME}_${service}" >&2 || true
            return 1
        fi
    done
    echo "+ All requested Swarm services report healthy."
}

# swarm_wait_for_proxy_ready DASHBOARD_URL [TIMEOUT_SECS] [AUTH_HEADER...]
#
# Verifies that the proxy dashboard HTTP endpoint is reachable and responsive.
swarm_wait_for_proxy_ready() {
    local dashboard_url="$1"
    local timeout="${2:-60}"
    shift 2 2>/dev/null || true
    local auth_header=("$@")

    local elapsed=0
    echo "Waiting up to ${timeout}s for PgVisor proxy dashboard at ${dashboard_url}..."
    while [ "${elapsed}" -lt "${timeout}" ]; do
        local http_code
        http_code=$(curl -s -o /dev/null -w "%{http_code}" "${auth_header[@]}" "${dashboard_url}/api/status" 2>/dev/null || echo "000")
        if [ "${http_code}" = "200" ] || [ "${http_code}" = "401" ] || [ "${http_code}" = "303" ]; then
            echo "+ PgVisor proxy dashboard is responsive (HTTP ${http_code})."
            return 0
        fi
        sleep 1
        elapsed=$((elapsed + 1))
    done

    echo "ERROR: Timed out waiting for PgVisor proxy dashboard at ${dashboard_url}" >&2
    return 1
}

# swarm_run_proxy_sql QUERY [MAX_ATTEMPTS] [PORT] [HOST]
#
# Executes a SQL query via the proxy using host psql if available,
# falling back to an in-network client container inside DinD.
swarm_run_proxy_sql() {
    local query="$1"
    local max_attempts="${2:-10}"
    local port="${3:-${SWARM_PROXY_PORT}}"
    local host="${4:-localhost}"

    local output=""
    local attempt
    for attempt in $(seq 1 "${max_attempts}"); do
        if command -v psql &> /dev/null; then
            if output=$(PGPASSWORD="" PGCONNECT_TIMEOUT=5 timeout 15 psql -h "${host}" -p "${port}" -U postgres -d postgres -t -A -c "${query}" 2>&1); then
                echo "${output}"
                return 0
            fi
        else
            if output=$(docker exec -i "${SWARM_DIND_CONTAINER}" docker run --rm \
                --network "${SWARM_STACK_NAME}_pgvisor-net" \
                -e PGPASSWORD="" \
                pgvisor-test-node:latest \
                psql -h pgvisor-proxy -p 5432 -U postgres -d postgres -t -A -c "${query}" 2>&1); then
                echo "${output}"
                return 0
            fi
        fi
        sleep 1
    done
    echo "${output}"
    return 1
}

# swarm_run_sql QUERY [MAX_ATTEMPTS] [PORT] [HOST]
#
# Alias for swarm_run_proxy_sql.
swarm_run_sql() {
    swarm_run_proxy_sql "$@"
}

# swarm_run_node_sql SERVICE_NAME QUERY
#
# Executes a SQL query directly on a specific service container in DinD.
swarm_run_node_sql() {
    local service_name="$1"
    local query="$2"

    local cid
    cid=$(swarm_find_container "${service_name}")
    if [ -z "${cid}" ]; then
        echo "ERROR: Container for service ${service_name} not found in DinD" >&2
        return 1
    fi

    docker exec -i "${SWARM_DIND_CONTAINER}" docker exec -i "${cid}" \
        psql -h localhost -U postgres -d postgres -t -A -c "${query}" 2>/dev/null || true
}

# swarm_get_sidecar_status SERVICE_NAME
#
# Queries the sidecar control status endpoint on a service container in DinD.
swarm_get_sidecar_status() {
    local service_name="$1"

    local cid
    cid=$(swarm_find_container "${service_name}")
    if [ -z "${cid}" ]; then
        return 1
    fi

    docker exec -i "${SWARM_DIND_CONTAINER}" docker exec -i "${cid}" \
        curl -s http://localhost:8080/control/status 2>/dev/null || true
}

# swarm_scale_service SERVICE_NAME REPLICAS
#
# Scales a service inside DinD Swarm.
swarm_scale_service() {
    local service_name="$1"
    local replicas="$2"

    docker exec "${SWARM_DIND_CONTAINER}" docker service scale "${SWARM_STACK_NAME}_${service_name}=${replicas}" >/dev/null
}

# swarm_cleanup [DIND_NAME]
#
# Cleanup handler for EXIT traps in swarm test scripts.
swarm_cleanup() {
    local dind_name="${1:-${SWARM_DIND_CONTAINER}}"
    if [ -n "${dind_name}" ]; then
        echo "Cleaning up Swarm DinD container: ${dind_name}..."
        swarm_down "${dind_name}"
    fi
}
