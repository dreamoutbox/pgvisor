#!/usr/bin/env bash
set -euo pipefail

# PgVisor Integration Test Runner
# Runs all test scripts in tests/*.sh sequentially.
# Resets the cluster before each test via ./reset-docker-compose.sh to guarantee fresh state.
# Fails fast on the first failing test (set -e).

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "${SCRIPT_DIR}"

# Ensure Ctrl+C terminates all child processes and exits immediately
cleanup() {
    echo ""
    echo "========================================================="
    echo "  Test suite interrupted by user (Ctrl+C). Aborting..."
    echo "========================================================="
    trap - INT TERM EXIT
    kill -- -$$ 2>/dev/null || true
    exit 130
}
trap cleanup INT TERM

FIRST_RESET_ARGS=""
for arg in "$@"; do
    case "$arg" in
        --build)
            FIRST_RESET_ARGS="--build"
            ;;
    esac
done

# Collect all test scripts matching tests/*.sh
shopt -s nullglob
TEST_FILES=(tests/*.sh)
shopt -u nullglob

if [ ${#TEST_FILES[@]} -eq 0 ]; then
    echo "Error: No test scripts found matching tests/*.sh"
    exit 1
fi

TOTAL_TESTS=${#TEST_FILES[@]}
PASSED_COUNT=0

echo "========================================================="
echo "  PgVisor Test Suite Runner"
echo "========================================================="
echo "Found ${TOTAL_TESTS} test script(s) to execute:"
for t in "${TEST_FILES[@]}"; do
    echo "  - ${t}"
done
echo "========================================================="
echo ""

START_TIME=$(date +%s)

for idx in "${!TEST_FILES[@]}"; do
    test_script="${TEST_FILES[$idx]}"
    test_num=$((idx + 1))

    echo "========================================================="
    echo "  [${test_num}/${TOTAL_TESTS}] Preparing environment for: ${test_script}"
    echo "========================================================="

    # Reset cluster state before every test
    if [ -n "${FIRST_RESET_ARGS}" ]; then
        ./reset-docker-compose.sh ${FIRST_RESET_ARGS}
        FIRST_RESET_ARGS=""
    else
        ./reset-docker-compose.sh
    fi

    echo ""
    echo "========================================================="
    echo "  [${test_num}/${TOTAL_TESTS}] Executing: ${test_script}"
    echo "========================================================="

    TEST_START=$(date +%s)
    bash "${test_script}"
    TEST_END=$(date +%s)
    TEST_DURATION=$((TEST_END - TEST_START))

    echo ""
    echo "PASS: ${test_script} (${TEST_DURATION}s)"
    PASSED_COUNT=$((PASSED_COUNT + 1))
    echo ""
done

END_TIME=$(date +%s)
TOTAL_DURATION=$((END_TIME - START_TIME))

echo "========================================================="
echo "  PgVisor Test Suite Summary"
echo "========================================================="
echo "Result: ALL TESTS PASSED"
echo "Passed: ${PASSED_COUNT}/${TOTAL_TESTS}"
echo "Total Time: ${TOTAL_DURATION}s"
echo "========================================================="
