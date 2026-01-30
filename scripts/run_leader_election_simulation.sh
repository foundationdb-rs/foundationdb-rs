#!/usr/bin/env bash
set -e

# Number of iterations per test (default: 1, use 0 for infinite)
MAX_ITERATIONS=${1:-1}

# Create trace directory
mkdir -p ./target/traces

# Build in release mode
echo "Building in release mode..."
cargo build --release -p foundationdb-recipes-simulation

# All test configurations
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
        echo "----------------------"
        echo "Iteration $iteration"
        echo "----------------------"

        # Clean up traces from previous iteration to avoid accumulating terabytes of JSON
        rm -rf ./target/traces/*

        if ! fdbserver -r simulation -f "$test" -b on --trace-format json -L ./target/traces --logsize 1GiB; then
            echo "FAILED on iteration $iteration for $test"
            echo "Check traces in ./target/traces"
            exit 1
        fi

        echo "Iteration $iteration passed"
        iteration=$((iteration + 1))
    done
    echo "PASSED: $test"
done

echo ""
echo "=========================================="
echo "All tests passed!"
echo "=========================================="
