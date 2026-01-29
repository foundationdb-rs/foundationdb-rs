#!/usr/bin/env bash
#
# Run All Leader Election Tests
#
# This script runs all leader election simulation test configurations:
# - test_leader_election.toml: Basic functionality test
# - test_ballot_stress.toml: High contention ballot collision stress
# - test_rapid_leadership.toml: Fast leadership turnover
# - test_short_lease.toml: Short lease with timing edge cases
#
# Each test verifies 11 invariants:
# 1. DualPathValidation - Replay logs vs FDB state match
# 2. LeaderIsCandidate - Leader is registered candidate
# 3. CandidateTimestamps - No future timestamps
# 4. LeadershipSequence - Per-client monotonic op_nums
# 5. RegistrationCoverage - All leaders were registered
# 6. NoOverlappingLeadership - No split-brain
# 7. BallotValueBinding - Each ballot maps to one leader
# 8. FencingTokenMonotonicity - Ballots increase monotonically
# 9. GlobalBallotSuccession - New leader ballot > previous ballot
# 10. MutexLinearizability - Valid mutex model
# 11. LeaseOverlapCheck - No active lease overlaps
#
# Usage:
#   ./scripts/run_all_leader_election_tests.sh [MAX_ITERATIONS]
#
# Arguments:
#   MAX_ITERATIONS: Number of iterations per test (default: 1, 0 for infinite)
#
# Example:
#   ./scripts/run_all_leader_election_tests.sh 10   # Run each test 10 times
#   ./scripts/run_all_leader_election_tests.sh 0    # Run indefinitely

set -e

MAX_ITERATIONS=${1:-1}

mkdir -p ./target/traces

echo "Building in release mode..."
cargo build --release -p foundationdb-recipes-simulation

TESTS=(
    "foundationdb-recipes-simulation/test_leader_election.toml"
    "foundationdb-recipes-simulation/test_ballot_stress.toml"
    "foundationdb-recipes-simulation/test_rapid_leadership.toml"
    "foundationdb-recipes-simulation/test_short_lease.toml"
)

for test in "${TESTS[@]}"; do
    echo ""
    echo "=========================================="
    echo "Running: $test"
    echo "=========================================="

    iteration=1
    while [ "$MAX_ITERATIONS" -eq 0 ] || [ "$iteration" -le "$MAX_ITERATIONS" ]; do
        echo "Iteration $iteration..."
        if ! fdbserver -r simulation -f "$test" -b on --trace-format json -L ./target/traces --logsize 1GiB; then
            echo "FAILED on iteration $iteration for $test"
            echo "Check traces in ./target/traces"
            exit 1
        fi
        iteration=$((iteration + 1))
    done
    echo "PASSED: $test"
done

echo ""
echo "=========================================="
echo "All tests passed!"
echo "=========================================="
