#!/usr/bin/env bash
set -e

# Number of iterations (default: 1, use 0 for infinite)
MAX_ITERATIONS=${1:-1}

# Create trace directory
mkdir -p ./target/traces

# Build in release mode
echo "Building in release mode..."
cargo build --release -p foundationdb-recipes-simulation

# Run simulation in a loop
iteration=1
while [ "$MAX_ITERATIONS" -eq 0 ] || [ "$iteration" -le "$MAX_ITERATIONS" ]; do
    echo "----------------------"
    echo "Running iteration $iteration"
    echo "----------------------"

    # Clean up traces from previous iteration to avoid accumulating terabytes of JSON
    rm -rf ./target/traces/*

    if ! fdbserver -r simulation -f foundationdb-recipes-simulation/test_leader_election.toml -b on --trace-format json -L ./target/traces --logsize 1GiB; then
        echo "Simulation failed on iteration $iteration"
        echo "Check traces in ./target/traces"
        exit 1
    fi

    echo "Iteration $iteration passed"
    iteration=$((iteration + 1))
done
