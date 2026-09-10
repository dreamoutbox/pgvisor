#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Unified Integration Test Suite Runner
#
# Supports sequential execution (default) and concurrent/parallel execution
# via --parallel or -j <N>. All output is strictly clean plain text.
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${SCRIPT_DIR}"
TEST_DIR="${REPO_ROOT}/tests"
LOG_DIR="${TEST_DIR}/.logs"
mkdir -p "${LOG_DIR}"

PARALLEL=false
MAX_JOBS=2

# Parse CLI arguments
while [[ $# -gt 0 ]]; do
    case "$1" in
        --parallel)
            PARALLEL=true
            shift
            ;;
        -j)
            PARALLEL=true
            MAX_JOBS="$2"
            shift 2
            ;;
        --jobs=*)
            PARALLEL=true
            MAX_JOBS="${1#*=}"
            shift
            ;;
        -h|--help)
            echo "Usage: $0 [options]"
            echo ""
            echo "Options:"
            echo "  --parallel         Run tests concurrently (default max jobs: 2)"
            echo "  -j, --jobs=N       Set max parallel jobs (implies --parallel)"
            echo "  -h, --help         Show this help message"
            exit 0
            ;;
        *)
            echo "Unknown argument: $1"
            echo "Run with --help for usage."
            exit 1
            ;;
    esac
done

# Ensure compose profiles are up to date
"${REPO_ROOT}/scripts/generate-test-composes.sh" > /dev/null

TEST_SCRIPTS=(
    "test-cluster-crud.sh"
    "test-backup-restore.sh"
    "test-incremental-pitr.sh"
    "test-failover.sh"
    "test-auto-rejoin.sh"
    "test-rejoin-fenced.sh"
    "test-add-node.sh"
    "test-switchover.sh"
    "test-users-permissions.sh"
    "test-transaction.sh"
    "test-routing.sh"
    "test-audit-logs.sh"
    "test-double-failure.sh"
    "test-proxy-failover.sh"
)

TOTAL_TESTS="${#TEST_SCRIPTS[@]}"
START_TIME=$(date +%s)

echo "============================================================="
if [[ "${PARALLEL}" == "true" ]]; then
    echo "  PgVisor Integration Test Suite (Parallel: ${MAX_JOBS} jobs)"
else
    echo "  PgVisor Integration Test Suite (Sequential Execution)"
fi
echo "============================================================="
echo "Total tests to run: ${TOTAL_TESTS}"
echo "Log directory: ${LOG_DIR}"
echo ""

declare -A TEST_STATUS
declare -A TEST_DURATION

run_single_test() {
    local test_name="$1"
    local script_path="${TEST_DIR}/${test_name}"
    local log_file="${LOG_DIR}/${test_name%.sh}.log"
    local t_start
    local t_end
    local duration
    local rc

    t_start=$(date +%s)

    if bash "${script_path}" > "${log_file}" 2>&1; then
        rc=0
    else
        rc=$?
    fi

    t_end=$(date +%s)
    duration=$((t_end - t_start))

    if [[ ${rc} -eq 0 ]]; then
        echo "[PASSED ] ${test_name} (${duration}s)"
        echo "PASS:${duration}" > "${LOG_DIR}/${test_name%.sh}.result"
    else
        echo "[FAILED ] ${test_name} (${duration}s, exit: ${rc}) - See ${log_file}"
        echo "FAIL:${duration}:${rc}" > "${LOG_DIR}/${test_name%.sh}.result"
    fi
}

# Clean old result markers
rm -f "${LOG_DIR}"/*.result

# Determine initial concurrency limit
concurrency=1
if [[ "${PARALLEL}" == "true" ]]; then
    concurrency="${MAX_JOBS}"
fi

# Print initial status for all tests upfront
for ((i=0; i<TOTAL_TESTS; i++)); do
    if [[ ${i} -lt ${concurrency} ]]; then
        echo "[STARTED] ${TEST_SCRIPTS[i]}"
    else
        echo "[WAIT]    ${TEST_SCRIPTS[i]}"
    fi
done

if [[ "${PARALLEL}" == "true" ]]; then
    # Concurrency control using background jobs and job pool
    running=0
    for ((i=0; i<TOTAL_TESTS; i++)); do
        test_file="${TEST_SCRIPTS[i]}"
        # If this test was waiting initially, print [STARTED] as it launches now
        if [[ ${i} -ge ${concurrency} ]]; then
            echo "[STARTED] ${test_file}"
        fi
        run_single_test "${test_file}" &
        running=$((running + 1))
        if [[ ${running} -ge ${MAX_JOBS} ]]; then
            wait -n 2>/dev/null || wait
            running=$((running - 1))
        fi
    done
    wait
else
    # Sequential execution
    for ((i=0; i<TOTAL_TESTS; i++)); do
        test_file="${TEST_SCRIPTS[i]}"
        if [[ ${i} -ge ${concurrency} ]]; then
            echo "[STARTED] ${test_file}"
        fi
        run_single_test "${test_file}"
    done
fi

TOTAL_END=$(date +%s)
TOTAL_DURATION=$((TOTAL_END - START_TIME))

# Collate results
PASSED_COUNT=0
FAILED_COUNT=0
FAILED_TESTS=()

echo ""
echo "============================================================="
echo "  Test Results Summary"
echo "============================================================="

for test_file in "${TEST_SCRIPTS[@]}"; do
    res_file="${LOG_DIR}/${test_file%.sh}.result"
    log_file="${LOG_DIR}/${test_file%.sh}.log"
    if [[ -f "${res_file}" ]]; then
        raw=$(cat "${res_file}")
        status=$(echo "${raw}" | cut -d':' -f1)
        dur=$(echo "${raw}" | cut -d':' -f2)
        if [[ "${status}" == "PASS" ]]; then
            printf "  %-26s : PASSED (%ds)\n" "${test_file}" "${dur}"
            PASSED_COUNT=$((PASSED_COUNT + 1))
        else
            printf "  %-26s : FAILED (%ds) -> %s\n" "${test_file}" "${dur}" "${log_file}"
            FAILED_COUNT=$((FAILED_COUNT + 1))
            FAILED_TESTS+=("${test_file} (${dur}s): ${log_file}")
        fi
    else
        printf "  %-26s : UNKNOWN -> %s\n" "${test_file}" "${log_file}"
        FAILED_COUNT=$((FAILED_COUNT + 1))
        FAILED_TESTS+=("${test_file} (unknown): ${log_file}")
    fi
done

echo "============================================================="
printf "  Total: %d | Passed: %d | Failed: %d | Duration: %ds\n" \
    "${TOTAL_TESTS}" "${PASSED_COUNT}" "${FAILED_COUNT}" "${TOTAL_DURATION}"
echo "============================================================="

if [[ ${FAILED_COUNT} -eq 0 ]]; then
    echo "All tests passed successfully!"
    exit 0
else
    echo "Failed Tests (${FAILED_COUNT}):"
    for failed_item in "${FAILED_TESTS[@]}"; do
        echo "  - ${failed_item}"
    done
    echo "============================================================="
    echo "${FAILED_COUNT} test(s) failed. Check logs above."
    exit 1
fi
