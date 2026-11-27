#!/usr/bin/env bash
#
# Usage: scripts/bisect_bindingtester.sh HARNESS_ARGS...
#
# Shrinks a failing bindingtester seed to its minimal number of operations, then
# prints the instructions of that minimal test. Pass the same arguments as the
# failing run (scripts/run_bindingtester.sh prints them), for example:
#
#   scripts/bisect_bindingtester.sh --num-ops 1000 --api-version 740 \
#     --test-name api --compare python rust --seed 3534790651
#
# --seed is required. The harness cannot bisect concurrent tests (--concurrency > 1).
# Add --all to also print the PUSH and SWAP instructions.

set -euo pipefail

if [[ " $* " != *" --seed "* ]]; then
  echo "usage: $0 HARNESS_ARGS... --seed SEED" >&2
  exit 2
fi

cd "$(pwd)/target/foundationdb_build/foundationdb"
exec ./bindings/bindingtester/bindingtester.py "$@" --bisect --print-test
