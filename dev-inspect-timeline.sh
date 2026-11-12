#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Cluster Timeline Inspection Utility
#
# Inspects PostgreSQL timeline IDs, checkpoint locations, replication status,
# and timeline branching history across all nodes in the cluster.
# Detects timeline divergence between leader and standby replicas.
#
# All output is strictly clean plain text (no ANSI escape codes).
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Configuration with environment variable overrides
CONTAINER_PREFIX="${PGVISOR_CONTAINER_PREFIX:-}"
CONTAINERS_ARG=""
PROXY_HOST="${PGVISOR_HOST:-}"
PROXY_PORT="${PGVISOR_PORT:-5432}"
CLUSTER_NAME="${PGVISOR_CLUSTER_NAME:-pgvisor-cluster}"
CHECK_S3=false
OUTPUT_JSON=false

# Shows usage and options for dev-inspect-timeline.sh.
#
# Arguments:
#   None
#
# Example usage:
#   usage
usage() {
    cat << EOF
Usage: $0 [options]

Inspects current timeline IDs and branching history across cluster nodes.

Options:
  -p, --prefix=PREFIX      Container name prefix to discover (e.g. 'pgvisor-node')
  -c, --containers=LIST    Explicit container names (comma- or space-separated)
      --proxy-host=HOST    Query timeline via proxy host (e.g. 127.0.0.1)
      --proxy-port=PORT    Proxy port (default: 5432)
  -s, --s3                 Also inspect archived timeline history files in S3
      --cluster=NAME       Cluster name for S3 lookup (default: pgvisor-cluster)
  -j, --json               Output in JSON format
  -h, --help               Show this help message

Example usage:
  $0
  $0 --prefix=pgvisor-node
  $0 --containers="pgvisor-node1 pgvisor-node2 pgvisor-node3"
  $0 --s3
  $0 --json
EOF
    exit 0
}

# Parse CLI options
while [[ $# -gt 0 ]]; do
    case "$1" in
        -p|--prefix)
            CONTAINER_PREFIX="$2"
            shift 2
            ;;
        --prefix=*)
            CONTAINER_PREFIX="${1#*=}"
            shift
            ;;
        -c|--containers)
            CONTAINERS_ARG="$2"
            shift 2
            ;;
        --containers=*)
            CONTAINERS_ARG="${1#*=}"
            shift
            ;;
        --proxy-host)
            PROXY_HOST="$2"
            shift 2
            ;;
        --proxy-host=*)
            PROXY_HOST="${1#*=}"
            shift
            ;;
        --proxy-port)
            PROXY_PORT="$2"
            shift 2
            ;;
        --proxy-port=*)
            PROXY_PORT="${1#*=}"
            shift
            ;;
        -s|--s3)
            CHECK_S3=true
            shift
            ;;
        --cluster)
            CLUSTER_NAME="$2"
            shift 2
            ;;
        --cluster=*)
            CLUSTER_NAME="${1#*=}"
            shift
            ;;
        -j|--json)
            OUTPUT_JSON=true
            shift
            ;;
        -h|--help)
            usage
            ;;
        *)
            echo "Unknown option: $1" >&2
            echo "Run '$0 --help' for usage." >&2
            exit 1
            ;;
    esac
done

# Discovers target Docker container names.
#
# Arguments:
#   $1 - Optional explicit container list
#   $2 - Optional container prefix
#
# Output:
#   Prints discovered container names, one per line.
#
# Example usage:
#   discover_containers "" "pgvisor-node"
discover_containers() {
    local explicit_list="$1"
    local prefix="$2"

    if [ -n "${explicit_list}" ]; then
        echo "${explicit_list}" | tr ',' ' ' | tr ' ' '\n' | grep -v '^$'
        return 0
    fi

    if [ -n "${prefix}" ]; then
        docker ps --filter "name=${prefix}" --format "{{.Names}}" 2>/dev/null | sort
        return 0
    fi

    # Auto-detect running node containers
    local found
    found=$(docker ps --format "{{.Names}}" 2>/dev/null | grep -E "pgvisor.*node|node[0-9]+" | sort || true)
    if [ -n "${found}" ]; then
        echo "${found}"
        return 0
    fi

    # Fallback to stopped/all containers matching pattern
    docker ps -a --format "{{.Names}}" 2>/dev/null | grep -E "pgvisor.*node|node[0-9]+" | sort || true
}

# Inspects PostgreSQL timeline information for a single Docker container.
#
# Arguments:
#   $1 - Container name
#
# Output:
#   Prints TSV: container \t timeline_id \t checkpoint_lsn \t role \t status
#
# Example usage:
#   inspect_container_timeline "pgvisor-node1"
inspect_container_timeline() {
    local container="$1"

    # Check container existence
    if ! docker inspect "${container}" > /dev/null 2>&1; then
        printf "%s\t-\t-\tunknown\tcontainer_not_found\n" "${container}"
        return 0
    fi

    local is_running
    is_running=$(docker inspect -f '{{.State.Running}}' "${container}" 2>/dev/null || echo "false")
    if [ "${is_running}" != "true" ]; then
        printf "%s\t-\t-\tstopped\tcontainer_stopped\n" "${container}"
        return 0
    fi

    # 1. Attempt live SQL query via psql on unix socket /tmp
    local sql_query="SELECT timeline_id, checkpoint_lsn, pg_is_in_recovery() FROM pg_control_checkpoint();"
    local sql_res
    if sql_res=$(docker exec "${container}" psql -h /tmp -U postgres -d postgres -t -A -F '|' -c "${sql_query}" 2>/dev/null); then
        if [ -n "${sql_res}" ]; then
            local tl_id chk_lsn in_rec role
            tl_id=$(echo "${sql_res}" | cut -d'|' -f1)
            chk_lsn=$(echo "${sql_res}" | cut -d'|' -f2)
            in_rec=$(echo "${sql_res}" | cut -d'|' -f3)

            if [ "${in_rec}" = "t" ]; then
                role="standby"
            else
                role="leader"
            fi
            printf "%s\t%s\t%s\t%s\trunning\n" "${container}" "${tl_id}" "${chk_lsn}" "${role}"
            return 0
        fi
    fi

    # 2. Fallback to pg_controldata inspection on disk
    local controldata
    controldata=$(docker exec "${container}" pg_controldata /var/lib/postgresql/data/pgdata 2>/dev/null || true)
    if [ -n "${controldata}" ]; then
        local c_tl c_lsn c_state role status
        c_tl=$(echo "${controldata}" | grep "Latest checkpoint's TimeLineID:" | awk '{print $NF}' || echo "-")
        c_lsn=$(echo "${controldata}" | grep "Latest checkpoint's REDO location:" | awk '{print $NF}' || echo "-")
        c_state=$(echo "${controldata}" | grep "Database cluster state:" | cut -d':' -f2- | xargs || echo "unknown")

        if [[ "${c_state}" == *"archive recovery"* ]] || [[ "${c_state}" == *"standby"* ]]; then
            role="standby"
        elif [[ "${c_state}" == *"production"* ]]; then
            role="leader"
        else
            role="recovering"
        fi
        status="${c_state}"

        printf "%s\t%s\t%s\t%s\t%s\n" "${container}" "${c_tl}" "${c_lsn}" "${role}" "${status}"
        return 0
    fi

    printf "%s\t-\t-\tunknown\tunreachable\n" "${container}"
}

# Inspects timeline history files (.history) inside a container's pg_wal directory.
#
# Arguments:
#   $1 - Container name
#
# Output:
#   Prints TSV: target_timeline \t parent_timeline \t switchpoint_lsn \t reason
#
# Example usage:
#   inspect_container_history "pgvisor-node1"
inspect_container_history() {
    local container="$1"
    local hist_files
    hist_files=$(docker exec "${container}" find /var/lib/postgresql/data/pgdata/pg_wal/ -maxdepth 1 -name "*.history" 2>/dev/null || true)

    if [ -z "${hist_files}" ]; then
        return 0
    fi

    while IFS= read -r f; do
        if [ -n "${f}" ]; then
            local fname
            fname=$(basename "${f}")
            # Target timeline derived from filename (e.g. 00000002.history -> 2)
            local hex_tl="${fname%%.history}"
            local target_tl
            target_tl=$((16#${hex_tl}))

            local content
            content=$(docker exec "${container}" cat "${f}" 2>/dev/null || true)
            while IFS= read -r line; do
                if [ -n "${line}" ] && [[ ! "${line}" =~ ^[[:space:]]*# ]]; then
                    local parent_tl switch_lsn reason
                    parent_tl=$(echo "${line}" | awk '{print $1}')
                    switch_lsn=$(echo "${line}" | awk '{print $2}')
                    reason=$(echo "${line}" | cut -d$'\t' -f3- | sed 's/^[[:space:]]*//')
                    printf "%s\t%s\t%s\t%s\n" "${target_tl}" "${parent_tl}" "${switch_lsn}" "${reason}"
                fi
            done <<< "${content}"
        fi
    done <<< "${hist_files}"
}

# Inspects timeline history files (.history) archived in S3.
#
# Arguments:
#   $1 - Cluster name
#
# Output:
#   Prints TSV: target_timeline \t parent_timeline \t switchpoint_lsn \t reason
#
# Example usage:
#   inspect_s3_history "pgvisor-cluster"
inspect_s3_history() {
    local cluster="$1"
    local s3_common="${SCRIPT_DIR}/scripts/lib/s3-common.sh"
    if [ ! -f "${s3_common}" ]; then
        return 0
    fi

    # shellcheck source=scripts/lib/s3-common.sh
    source "${s3_common}"

    local prefix="clusters/${cluster}/wal/"
    local keys
    keys=$(s3_list_all_keys "${S3_BUCKET}" "${prefix}")
    if [ -z "${keys}" ]; then
        return 0
    fi

    local tmp_dir="/tmp/pgvisor-s3-hist-$$"
    mkdir -p "${tmp_dir}"

    while IFS= read -r key; do
        if [[ "${key}" == *.history ]]; then
            local fname
            fname=$(basename "${key}")
            local hex_tl="${fname%%.history}"
            local target_tl=$((16#${hex_tl}))
            local local_dest="${tmp_dir}/${fname}"

            if s3_download_object "${key}" "${local_dest}" "${S3_BUCKET}" 2>/dev/null; then
                while IFS= read -r line; do
                    if [ -n "${line}" ] && [[ ! "${line}" =~ ^[[:space:]]*# ]]; then
                        local parent_tl switch_lsn reason
                        parent_tl=$(echo "${line}" | awk '{print $1}')
                        switch_lsn=$(echo "${line}" | awk '{print $2}')
                        reason=$(echo "${line}" | cut -d$'\t' -f3- | sed 's/^[[:space:]]*//')
                        printf "%s\t%s\t%s\t%s\n" "${target_tl}" "${parent_tl}" "${switch_lsn}" "${reason}"
                    fi
                done < "${local_dest}"
            fi
        fi
    done <<< "${keys}"

    rm -rf "${tmp_dir}"
}

# Collect discovered containers
CONTAINERS=()
while IFS= read -r c; do
    if [ -n "${c}" ]; then
        CONTAINERS+=("${c}")
    fi
done < <(discover_containers "${CONTAINERS_ARG}" "${CONTAINER_PREFIX}")

# Query via proxy if specified or if no containers found
PROXY_RESULT=""
if [ -n "${PROXY_HOST}" ] || [ ${#CONTAINERS[@]} -eq 0 ]; then
    PHOST="${PROXY_HOST:-127.0.0.1}"
    PPORT="${PROXY_PORT:-5432}"
    if command -v psql > /dev/null 2>&1; then
        PROXY_QUERY="SELECT timeline_id, checkpoint_lsn, pg_is_in_recovery() FROM pg_control_checkpoint();"
        if P_RES=$(PGPASSWORD="${PGPASSWORD:-postgres}" psql -h "${PHOST}" -p "${PPORT}" -U postgres -d postgres -t -A -F '|' -c "${PROXY_QUERY}" 2>/dev/null); then
            if [ -n "${P_RES}" ]; then
                P_TL=$(echo "${P_RES}" | cut -d'|' -f1)
                P_LSN=$(echo "${P_RES}" | cut -d'|' -f2)
                P_REC=$(echo "${P_RES}" | cut -d'|' -f3)
                P_ROLE="leader"
                if [ "${P_REC}" = "t" ]; then
                    P_ROLE="standby"
                fi
                PROXY_RESULT=$(printf "proxy (%s:%s)\t%s\t%s\t%s\trunning\n" "${PHOST}" "${PPORT}" "${P_TL}" "${P_LSN}" "${P_ROLE}")
            fi
        fi
    fi
fi

if [ ${#CONTAINERS[@]} -eq 0 ] && [ -z "${PROXY_RESULT}" ]; then
    echo "ERROR: No pgvisor containers or accessible PostgreSQL proxy found." >&2
    echo "Provide container names with --containers or specify proxy with --proxy-host." >&2
    exit 1
fi

# Inspect all node timelines
NODE_RECORDS=()
for container in "${CONTAINERS[@]}"; do
    rec=$(inspect_container_timeline "${container}")
    NODE_RECORDS+=("${rec}")
done
if [ -n "${PROXY_RESULT}" ] && [ ${#CONTAINERS[@]} -eq 0 ]; then
    NODE_RECORDS+=("${PROXY_RESULT}")
fi

# Collect history records (deduplicated)
HISTORY_RAW=""
for container in "${CONTAINERS[@]}"; do
    h_rec=$(inspect_container_history "${container}")
    if [ -n "${h_rec}" ]; then
        HISTORY_RAW=$(printf "%s\n%s" "${HISTORY_RAW}" "${h_rec}")
    fi
done

if [ "${CHECK_S3}" = true ]; then
    s3_h_rec=$(inspect_s3_history "${CLUSTER_NAME}")
    if [ -n "${s3_h_rec}" ]; then
        HISTORY_RAW=$(printf "%s\n%s" "${HISTORY_RAW}" "${s3_h_rec}")
    fi
fi

# Deduplicate history lines
HISTORY_UNIQUE=""
if [ -n "${HISTORY_RAW}" ]; then
    HISTORY_UNIQUE=$(echo "${HISTORY_RAW}" | sed '/^[[:space:]]*$/d' | sort -u -k1,1n -k2,2n)
fi

# Analyze timeline alignment
TIMELINES_SEEN=()
for rec in "${NODE_RECORDS[@]}"; do
    tl=$(echo "${rec}" | cut -f2)
    if [ "${tl}" != "-" ] && [ -n "${tl}" ]; then
        TIMELINES_SEEN+=("${tl}")
    fi
done

mapfile -t UNIQUE_TIMELINES < <(printf "%s\n" "${TIMELINES_SEEN[@]}" | sort -u)
ALIGNED=false
DIVERGENCE=false
CLUSTER_TL="-"

if [ ${#UNIQUE_TIMELINES[@]} -eq 1 ]; then
    ALIGNED=true
    CLUSTER_TL="${UNIQUE_TIMELINES[0]}"
elif [ ${#UNIQUE_TIMELINES[@]} -gt 1 ]; then
    DIVERGENCE=true
fi

# Output JSON format if requested
if [ "${OUTPUT_JSON}" = true ]; then
    if ! command -v jq > /dev/null 2>&1; then
        echo "ERROR: 'jq' is required for JSON output mode." >&2
        exit 1
    fi

    # Build nodes JSON array
    NODES_JSON="[]"
    for rec in "${NODE_RECORDS[@]}"; do
        c_name=$(echo "${rec}" | cut -f1)
        c_tl=$(echo "${rec}" | cut -f2)
        c_lsn=$(echo "${rec}" | cut -f3)
        c_role=$(echo "${rec}" | cut -f4)
        c_status=$(echo "${rec}" | cut -f5)

        NODE_OBJ=$(jq -n \
            --arg node "${c_name}" \
            --arg timeline "${c_tl}" \
            --arg lsn "${c_lsn}" \
            --arg role "${c_role}" \
            --arg status "${c_status}" \
            '{node: $node, timeline_id: (if $timeline == "-" then null else ($timeline | tonumber) end), checkpoint_lsn: (if $lsn == "-" then null else $lsn end), role: $role, status: $status}')

        NODES_JSON=$(echo "${NODES_JSON}" | jq --argjson item "${NODE_OBJ}" '. += [$item]')
    done

    # Build history JSON array
    HIST_JSON="[]"
    if [ -n "${HISTORY_UNIQUE}" ]; then
        while IFS=$'\t' read -r tgt_tl prnt_tl sw_lsn rsn; do
            if [ -n "${tgt_tl}" ]; then
                HIST_OBJ=$(jq -n \
                    --arg target "${tgt_tl}" \
                    --arg parent "${prnt_tl}" \
                    --arg lsn "${sw_lsn}" \
                    --arg reason "${rsn}" \
                    '{timeline: ($target | tonumber), parent_timeline: ($parent | tonumber), switchpoint_lsn: $lsn, reason: $reason}')
                HIST_JSON=$(echo "${HIST_JSON}" | jq --argjson item "${HIST_OBJ}" '. += [$item]')
            fi
        done <<< "${HISTORY_UNIQUE}"
    fi

    jq -n \
        --arg aligned "${ALIGNED}" \
        --arg divergence "${DIVERGENCE}" \
        --arg cluster_tl "${CLUSTER_TL}" \
        --argjson nodes "${NODES_JSON}" \
        --argjson history "${HIST_JSON}" \
        '{
            aligned: ($aligned == "true"),
            divergence: ($divergence == "true"),
            cluster_timeline: (if $cluster_tl == "-" then null else ($cluster_tl | tonumber) end),
            nodes: $nodes,
            history: $history
        }'
    exit 0
fi

# Print plain-text formatted output
echo "================================================================================"
echo "  PgVisor Cluster Timeline Inspection"
echo "================================================================================"
echo ""
echo "[Node Timelines]"
printf "%-25s  %-10s  %-10s  %-18s  %-15s\n" "NODE / CONTAINER" "ROLE" "TIMELINE" "CHECKPOINT LSN" "STATUS"
printf "%-25s  %-10s  %-10s  %-18s  %-15s\n" "-------------------------" "----------" "----------" "------------------" "---------------"

for rec in "${NODE_RECORDS[@]}"; do
    c_name=$(echo "${rec}" | cut -f1)
    c_tl=$(echo "${rec}" | cut -f2)
    c_lsn=$(echo "${rec}" | cut -f3)
    c_role=$(echo "${rec}" | cut -f4)
    c_status=$(echo "${rec}" | cut -f5)

    printf "%-25s  %-10s  %-10s  %-18s  %-15s\n" \
        "${c_name}" "${c_role}" "${c_tl}" "${c_lsn}" "${c_status}"
done

echo ""
echo "[Cluster Alignment]"
if [ "${ALIGNED}" = true ]; then
    echo "Status: ALIGNED (All nodes on Timeline ${CLUSTER_TL})"
elif [ "${DIVERGENCE}" = true ]; then
    echo "WARNING: TIMELINE DIVERGENCE DETECTED across cluster nodes!"
    for utl in "${UNIQUE_TIMELINES[@]}"; do
        nodes_on_tl=()
        for rec in "${NODE_RECORDS[@]}"; do
            if [ "$(echo "${rec}" | cut -f2)" = "${utl}" ]; then
                nodes_on_tl+=("$(echo "${rec}" | cut -f1)")
            fi
        done
        echo "  - Timeline ${utl}: $(IFS=, ; echo "${nodes_on_tl[*]}")"
    done
    echo "Action needed: Nodes on older or divergent timelines must re-sync from leader."
else
    echo "Status: UNKNOWN (Unable to retrieve timeline from running nodes)"
fi

if [ -n "${HISTORY_UNIQUE}" ]; then
    echo ""
    echo "[Timeline Branching History]"
    while IFS=$'\t' read -r tgt_tl prnt_tl sw_lsn rsn; do
        if [ -n "${tgt_tl}" ]; then
            echo "  Timeline ${tgt_tl} branched from Timeline ${prnt_tl} at LSN ${sw_lsn} (${rsn})"
        fi
    done <<< "${HISTORY_UNIQUE}"
fi
echo ""
