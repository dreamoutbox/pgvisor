#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Developer Leader Read-Write & Follower Read-Only Test Script
#
# Verifies directly against each cluster node container:
#   1. Exactly one leader (sidecar role=leader, pg_is_in_recovery=f) and at
#      least one follower (sidecar role!=leader, pg_is_in_recovery=t).
#   2. Leader node supports full read-write (CREATE, INSERT, SELECT, UPDATE, DELETE).
#   3. Every follower node supports reads (and observes replicated leader data)
#      and REJECTS all write operations (INSERT, UPDATE, DELETE, CREATE TABLE)
#      with a read-only transaction error.
#
# If any follower node accepts a write, the script fails with a non-zero exit code.
# All output is strictly clean plain text (no ANSI escape codes).
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/lib/helper.sh
source "${SCRIPT_DIR}/tests/lib/helper.sh"

# Configuration with environment variable overrides
NODES_CSV="${PGVISOR_NODES:-pgvisor-node1,pgvisor-node2,pgvisor-node3}"
PGUSER="${PGUSER:-postgres}"
PGDATABASE="${PGDATABASE:-postgres}"
PROBE_TABLE="${PGVISOR_PROBE_TABLE:-pgvisor_dev_rw_probe}"
REPLICATION_WAIT_SECS="${PGVISOR_REPLICATION_WAIT_SECS:-10}"
KEEP_TABLE=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --nodes=*)
            NODES_CSV="${1#*=}"
            shift
            ;;
        --user=*)
            PGUSER="${1#*=}"
            shift
            ;;
        --db=*)
            PGDATABASE="${1#*=}"
            shift
            ;;
        --table=*)
            PROBE_TABLE="${1#*=}"
            shift
            ;;
        --wait=*)
            REPLICATION_WAIT_SECS="${1#*=}"
            shift
            ;;
        --keep-table)
            KEEP_TABLE=true
            shift
            ;;
        -h|--help)
            echo "Usage: $0 [options]"
            echo ""
            echo "Tests read-write access on the leader node and enforces read-only"
            echo "behavior on every follower node. Fails if any follower accepts writes."
            echo ""
            echo "Options:"
            echo "  --nodes=N1,N2,N3   Comma-separated Docker node containers"
            echo "                     (default: pgvisor-node1,pgvisor-node2,pgvisor-node3)"
            echo "  --user=USER        PostgreSQL user (default: postgres)"
            echo "  --db=DB            PostgreSQL database (default: postgres)"
            echo "  --table=TABLE      Probe table name (default: pgvisor_dev_rw_probe)"
            echo "  --wait=SECONDS     Max seconds to wait for replication to followers (default: 10)"
            echo "  --keep-table       Do not drop the probe table on exit"
            echo "  -h, --help         Show this help message"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            echo "Run '$0 --help' for usage."
            exit 1
            ;;
    esac
done

IFS=',' read -r -a NODES <<< "${NODES_CSV}"

LEADER_NODE=""
FOLLOWER_NODES=()
FAILURES=0
TABLE_CREATED=false
PROBE_TOKEN="rw_probe_$(date +%s)_$$"
UPDATED_TOKEN="${PROBE_TOKEN}_updated"

# Run SQL directly inside a node container with ON_ERROR_STOP=1.
# Captures stdout+stderr and preserves psql's exit code.
exec_node_psql() {
    local container="$1"
    local query="$2"
    docker exec -i "${container}" psql \
        -h 127.0.0.1 \
        -U "${PGUSER}" \
        -d "${PGDATABASE}" \
        -v ON_ERROR_STOP=1 \
        -q -t -A \
        -c "${query}" 2>&1
}

# Assert that a write query on a follower node FAILS with a read-only error.
assert_follower_rejects_write() {
    local follower="$1"
    local label="$2"
    local query="$3"
    local output=""

    if output=$(exec_node_psql "${follower}" "${query}"); then
        echo "  FAIL [${follower}]: ${label} SUCCEEDED on follower node! Output: ${output}"
        FAILURES=$((FAILURES + 1))
        return 1
    fi

    if echo "${output}" | grep -qi "read-only transaction"; then
        echo "  PASS [${follower}]: ${label} rejected (read-only transaction)."
        return 0
    fi

    echo "  FAIL [${follower}]: ${label} failed, but NOT with a read-only transaction error:"
    echo "       ${output}"
    FAILURES=$((FAILURES + 1))
    return 1
}

cleanup() {
    if [[ "${KEEP_TABLE}" == "true" ]] || [[ "${TABLE_CREATED}" != "true" ]] || [[ -z "${LEADER_NODE}" ]]; then
        return 0
    fi
    exec_node_psql "${LEADER_NODE}" "DROP TABLE IF EXISTS ${PROBE_TABLE};" > /dev/null 2>&1 || true
}
trap cleanup EXIT

echo "============================================================="
echo "  PgVisor: Leader Read-Write & Follower Read-Only Check"
echo "  Nodes: ${NODES_CSV}"
echo "  DB: ${PGDATABASE} | User: ${PGUSER} | Table: ${PROBE_TABLE}"
echo "============================================================="
echo ""

# ------------------------------------------------------------------------------
# Step 1: Discover running nodes, sidecar roles, and pg_is_in_recovery() state
# ------------------------------------------------------------------------------
echo "[Step 1/3] Inspecting node roles and PostgreSQL recovery state..."

RUNNING_CONTAINERS=$(docker ps --format '{{.Names}}' 2>/dev/null || true)
LEADERS=()

for node in "${NODES[@]}"; do
    node="${node//[[:space:]]/}"
    if [[ -z "${node}" ]]; then
        continue
    fi

    if ! echo "${RUNNING_CONTAINERS}" | grep -qx "${node}"; then
        echo "  FAIL [${node}]: container is not running."
        FAILURES=$((FAILURES + 1))
        continue
    fi

    sidecar_role=$(get_sidecar_status "${node}" | json_extract "role" || true)
    sidecar_role="${sidecar_role:-unknown}"

    recovery_state=""
    if ! recovery_state=$(exec_node_psql "${node}" "SELECT pg_is_in_recovery();"); then
        echo "  FAIL [${node}]: unable to query PostgreSQL (sidecar role=${sidecar_role}): ${recovery_state}"
        FAILURES=$((FAILURES + 1))
        continue
    fi
    recovery_state=$(echo "${recovery_state}" | tr -d '[:space:]')

    echo "  Node ${node}: sidecar_role=${sidecar_role}, pg_is_in_recovery=${recovery_state}"

    if [[ "${sidecar_role}" == "leader" ]] || [[ "${sidecar_role}" == "unknown" && "${recovery_state}" == "f" ]]; then
        LEADERS+=("${node}")
        if [[ "${recovery_state}" != "f" ]]; then
            echo "  FAIL [${node}]: sidecar reports leader, but pg_is_in_recovery=${recovery_state} (expected 'f')."
            FAILURES=$((FAILURES + 1))
        fi
    else
        FOLLOWER_NODES+=("${node}")
        if [[ "${recovery_state}" != "t" ]]; then
            echo "  FAIL [${node}]: follower has pg_is_in_recovery=${recovery_state} (expected 't' - possible split-brain!)."
            FAILURES=$((FAILURES + 1))
        fi
    fi
done

if [[ "${#LEADERS[@]}" -eq 0 ]]; then
    echo "FAIL: No leader node detected. Ensure the cluster is running (e.g. docker compose up -d)."
    exit 1
fi

if [[ "${#LEADERS[@]}" -gt 1 ]]; then
    echo "FAIL: Multiple leader nodes detected (${LEADERS[*]}) - split-brain!"
    exit 1
fi

LEADER_NODE="${LEADERS[0]}"

if [[ "${#FOLLOWER_NODES[@]}" -eq 0 ]]; then
    echo "FAIL: No follower nodes detected to verify read-only enforcement."
    exit 1
fi

echo "  Topology: leader=${LEADER_NODE}, followers=${FOLLOWER_NODES[*]}"
echo ""

# ------------------------------------------------------------------------------
# Step 2: Verify Read-Write on Leader Node
# ------------------------------------------------------------------------------
echo "[Step 2/3] Testing read-write operations on leader (${LEADER_NODE})..."

if out=$(exec_node_psql "${LEADER_NODE}" "CREATE TABLE IF NOT EXISTS ${PROBE_TABLE} (id SERIAL PRIMARY KEY, token TEXT NOT NULL, created_at TIMESTAMPTZ NOT NULL DEFAULT now());"); then
    TABLE_CREATED=true
    echo "  PASS [${LEADER_NODE}]: CREATE TABLE succeeded."
else
    echo "  FAIL [${LEADER_NODE}]: CREATE TABLE failed: ${out}"
    exit 1
fi

if inserted_id=$(exec_node_psql "${LEADER_NODE}" "INSERT INTO ${PROBE_TABLE} (token) VALUES ('${PROBE_TOKEN}') RETURNING id;"); then
    echo "  PASS [${LEADER_NODE}]: INSERT succeeded (id=${inserted_id})."
else
    echo "  FAIL [${LEADER_NODE}]: INSERT failed: ${inserted_id}"
    FAILURES=$((FAILURES + 1))
fi

if read_back=$(exec_node_psql "${LEADER_NODE}" "SELECT token FROM ${PROBE_TABLE} WHERE token = '${PROBE_TOKEN}';") && [[ "${read_back}" == "${PROBE_TOKEN}" ]]; then
    echo "  PASS [${LEADER_NODE}]: SELECT after INSERT returned expected row."
else
    echo "  FAIL [${LEADER_NODE}]: SELECT after INSERT failed or mismatched (got: '${read_back}')."
    FAILURES=$((FAILURES + 1))
fi

if out=$(exec_node_psql "${LEADER_NODE}" "UPDATE ${PROBE_TABLE} SET token = '${UPDATED_TOKEN}' WHERE token = '${PROBE_TOKEN}';"); then
    echo "  PASS [${LEADER_NODE}]: UPDATE succeeded."
else
    echo "  FAIL [${LEADER_NODE}]: UPDATE failed: ${out}"
    FAILURES=$((FAILURES + 1))
fi

if read_updated=$(exec_node_psql "${LEADER_NODE}" "SELECT token FROM ${PROBE_TABLE} WHERE token = '${UPDATED_TOKEN}';") && [[ "${read_updated}" == "${UPDATED_TOKEN}" ]]; then
    echo "  PASS [${LEADER_NODE}]: SELECT after UPDATE returned expected row."
else
    echo "  FAIL [${LEADER_NODE}]: SELECT after UPDATE failed or mismatched (got: '${read_updated}')."
    FAILURES=$((FAILURES + 1))
fi

echo ""

# ------------------------------------------------------------------------------
# Step 3: Verify Read-Only on Every Follower Node
# ------------------------------------------------------------------------------
echo "[Step 3/3] Testing read-only enforcement on follower node(s)..."

for follower in "${FOLLOWER_NODES[@]}"; do
    echo "  -- Checking follower: ${follower} --"

    # Poll briefly for streaming replication to replay the leader write
    replicated=false
    follower_val=""
    for _ in $(seq 1 "${REPLICATION_WAIT_SECS}"); do
        if follower_val=$(exec_node_psql "${follower}" "SELECT token FROM ${PROBE_TABLE} WHERE token = '${UPDATED_TOKEN}';") && [[ "${follower_val}" == "${UPDATED_TOKEN}" ]]; then
            replicated=true
            break
        fi
        sleep 1
    done

    if [[ "${replicated}" == "true" ]]; then
        echo "  PASS [${follower}]: SELECT succeeded and observed replicated row."
    else
        echo "  FAIL [${follower}]: SELECT did not observe replicated row within ${REPLICATION_WAIT_SECS}s (last output: '${follower_val}')."
        FAILURES=$((FAILURES + 1))
    fi

    illegal_table="${PROBE_TABLE}_illegal_${follower//-/_}"
    assert_follower_rejects_write "${follower}" "INSERT" \
        "INSERT INTO ${PROBE_TABLE} (token) VALUES ('illegal_write_on_${follower}');" || true
    assert_follower_rejects_write "${follower}" "UPDATE" \
        "UPDATE ${PROBE_TABLE} SET token = 'illegal_update_${follower}' WHERE token = '${UPDATED_TOKEN}';" || true
    assert_follower_rejects_write "${follower}" "DELETE" \
        "DELETE FROM ${PROBE_TABLE} WHERE token = '${UPDATED_TOKEN}';" || true
    if ! assert_follower_rejects_write "${follower}" "CREATE TABLE" \
        "CREATE TABLE ${illegal_table} (id INT);"; then
        exec_node_psql "${follower}" "DROP TABLE IF EXISTS ${illegal_table};" > /dev/null 2>&1 || true
    fi
done

# Verify DELETE also works on leader before cleanup
if out=$(exec_node_psql "${LEADER_NODE}" "DELETE FROM ${PROBE_TABLE} WHERE token = '${UPDATED_TOKEN}';"); then
    echo "  PASS [${LEADER_NODE}]: DELETE succeeded."
else
    echo "  FAIL [${LEADER_NODE}]: DELETE failed: ${out}"
    FAILURES=$((FAILURES + 1))
fi

echo ""
echo "============================================================="
if [[ "${FAILURES}" -eq 0 ]]; then
    echo "  RESULT: PASS (leader is read-write, all followers are read-only)"
    echo "============================================================="
    exit 0
else
    echo "  RESULT: FAIL (${FAILURES} check(s) failed)"
    echo "============================================================="
    exit 1
fi
