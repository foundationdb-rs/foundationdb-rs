#!/usr/bin/env bash
# Run the leader election simulation configurations under fdbserver.
#
# Usage:
#   ./scripts/run_leader_election_simulation.sh [ITERATIONS] [CONFIG]
#
#   ITERATIONS  how many times to run each configuration (default 1, 0 = forever)
#   CONFIG      a single configuration to run, by name (e.g. pause_fencing) or
#               by path; all five run when it is omitted
#
# Every iteration runs with an explicit seed, printed before the run starts: a
# failure is only useful if it can be replayed, and the command to replay it is
# printed when one happens.
set -e

MAX_ITERATIONS=${1:-1}
ONLY=${2:-}

CONFIGS=(
    "foundationdb-recipes-simulation/test_baseline.toml"
    "foundationdb-recipes-simulation/test_strict_mutex.toml"
    "foundationdb-recipes-simulation/test_short_lease_stress.toml"
    "foundationdb-recipes-simulation/test_churn_attrition.toml"
    "foundationdb-recipes-simulation/test_pause_fencing.toml"
)

if [ -n "$ONLY" ]; then
    if [ -f "$ONLY" ]; then
        CONFIGS=("$ONLY")
    elif [ -f "foundationdb-recipes-simulation/test_${ONLY}.toml" ]; then
        CONFIGS=("foundationdb-recipes-simulation/test_${ONLY}.toml")
    else
        echo "No such configuration: $ONLY"
        echo "Known configurations:"
        printf '  %s\n' "${CONFIGS[@]}"
        exit 2
    fi
fi

mkdir -p ./target/traces

echo "Building in release mode..."
cargo build --release -p foundationdb-recipes-simulation

for config in "${CONFIGS[@]}"; do
    echo ""
    echo "=========================================="
    echo "Running: $config"
    echo "=========================================="

    iteration=1
    while [ "$MAX_ITERATIONS" -eq 0 ] || [ "$iteration" -le "$MAX_ITERATIONS" ]; do
        # Chosen here rather than left to fdbserver, so that it can be printed
        # before the run rather than dug out of a trace after it.
        seed=$(( ((RANDOM << 15) | RANDOM) & 0x7fffffff ))
        echo "----------------------"
        echo "Iteration $iteration (seed $seed)"
        echo "----------------------"

        # Traces from an iteration that passed are of no use, and keeping every
        # one of them costs gigabytes.
        rm -rf ./target/traces/*

        if ! fdbserver -r simulation -f "$config" -b on --trace-format json \
            -L ./target/traces --logsize 1GiB -s "$seed"; then
            echo ""
            echo "FAILED on iteration $iteration for $config (seed $seed)"
            echo "Traces are in ./target/traces"
            echo "Reproduce with:"
            echo "  fdbserver -r simulation -f $config -b on --trace-format json \\"
            echo "    -L ./target/traces --logsize 1GiB -s $seed"
            exit 1
        fi

        echo "Iteration $iteration passed"
        iteration=$((iteration + 1))
    done
    echo "PASSED: $config"
done

echo ""
echo "=========================================="
echo "All configurations passed!"
echo "=========================================="
