#!/usr/bin/env bash
#
# Usage: scripts/run_bindingtester.sh [ITERATIONS]
#
# Runs the regression seeds, then ITERATIONS (default 1) rounds of random tests.
# A failing test does not stop the run: its repro command is printed right away,
# and again in a summary at the end. The script exits non-zero if any test failed.
#
# Environment:
#   BINDINGTESTER_SEED   Base seed. When set, iteration i, test k of the random loop
#                        uses seed BINDINGTESTER_SEED + 10 * i + k, so the run is
#                        reproducible. The coverage job uses this. Concurrent tests
#                        still depend on thread scheduling.
#   BINDINGTESTER_TRACE  Directory. When set, each test writes its harness and tester
#                        output and the FDB client trace files of both processes to
#                        its own subdirectory. RUST_LOG defaults to debug for the
#                        Rust tester. Subdirectories of passing tests are removed, so
#                        only failures are kept.
#   BINDINGTESTER_MAX_FAILURES
#                        Stop after this many failed runs (default 10), so a bug that
#                        fails every seed does not run the whole loop.

set -uo pipefail

fdb_api_version=740

fdb_rs_dir=$(pwd)
fdb_builddir=${fdb_rs_dir:?}/target/foundationdb_build
fdb_dir="${fdb_builddir:?}/foundationdb"
cd "${fdb_dir:?}" || exit 1

if [ -n "${BINDINGTESTER_TRACE:-}" ]; then
  mkdir -p "${BINDINGTESTER_TRACE}"
  BINDINGTESTER_TRACE=$(cd "${BINDINGTESTER_TRACE}" && pwd)
fi

failures=()
run_count=0
max_failures=${BINDINGTESTER_MAX_FAILURES:-10}

summary() {
  echo
  echo "${run_count} bindingtester runs, ${#failures[@]} failed."
  if [ "${#failures[@]}" -ne 0 ]; then
    echo "Failed runs, with the command to reproduce each one:"
    printf '  %s\n' "${failures[@]}"
    echo "Shrink a failure to its minimal number of operations with scripts/bisect_bindingtester.sh."
    exit 1
  fi
}

random_seed() {
  od -An -N4 -tu4 /dev/urandom | tr -d ' '
}

# run_test HARNESS_ARGS...
#
# Runs the harness once. A seed is drawn here when the arguments do not have one,
# so every run, including a failing one, has a known seed.
run_test() {
  local args=("$@")
  local test_name="" seed="" i
  for ((i = 0; i < ${#args[@]}; i++)); do
    case "${args[i]}" in
      --test-name) test_name="${args[i + 1]}" ;;
      --seed) seed="${args[i + 1]}" ;;
    esac
  done
  if [ -z "${seed}" ]; then
    seed=$(random_seed)
    args+=(--seed "${seed}")
  fi

  local cmd=(./bindings/bindingtester/bindingtester.py "${args[@]}")
  local repro
  repro="(cd $(printf '%q' "${fdb_dir}") && $(printf '%q ' "${cmd[@]}"))"
  run_count=$((run_count + 1))
  echo "+ ${repro}"

  local status logdir=""
  if [ -n "${BINDINGTESTER_TRACE:-}" ]; then
    logdir="${BINDINGTESTER_TRACE}/$(printf '%04d' "${run_count}")-${test_name}-${seed}"
    mkdir -p "${logdir}"
    FDB_NETWORK_OPTION_TRACE_ENABLE="${logdir}" RUST_LOG="${RUST_LOG:-debug}" \
      "${cmd[@]}" --enable-client-trace-logging "${logdir}" > "${logdir}/output.log" 2>&1
    status=$?
    tail -n 5 "${logdir}/output.log"
  else
    "${cmd[@]}"
    status=$?
  fi

  if [ "${status}" -ne 0 ]; then
    echo "FAILED (exit ${status}): ${test_name} seed ${seed}"
    echo "Reproduce with: ${repro}"
    if [ -n "${logdir}" ]; then
      echo "Logs and traces: ${logdir}"
      failures+=("${repro}    # logs: ${logdir}")
    else
      failures+=("${repro}")
    fi
    if [ "${#failures[@]}" -ge "${max_failures}" ]; then
      echo "Stopping after ${max_failures} failed runs."
      summary
    fi
  elif [ -n "${logdir}" ]; then
    rm -rf "${logdir}"
  fi
}

# These are faulty seeds, for now, it is good to check them all the time to avoid regression.
run_test --num-ops 1000 --api-version $fdb_api_version --test-name api --compare python rust --seed 3534790651
run_test --num-ops 1000 --api-version $fdb_api_version --test-name api --compare python rust --seed 3864917676
run_test --num-ops 1000 --api-version $fdb_api_version --test-name api --concurrency 5 rust --seed 3153055325
run_test --num-ops 1000 --api-version $fdb_api_version --test-name directory --concurrency 1 rust --no-directory-snapshot-ops --compare python --seed 2095856910
run_test --num-ops 1000 --api-version $fdb_api_version --test-name directory --concurrency 1 rust --no-directory-snapshot-ops --compare python --seed 2483220251
run_test --num-ops 1000 --api-version $fdb_api_version --test-name directory --concurrency 1 rust --no-directory-snapshot-ops --compare python --seed 241550211
run_test --num-ops 1000 --api-version $fdb_api_version --test-name directory --concurrency 1 rust --no-directory-snapshot-ops --compare python --seed 1508488514
run_test --num-ops 1000 --api-version $fdb_api_version --test-name directory --concurrency 1 rust --no-directory-snapshot-ops --compare python --seed 4203526853

# https://github.com/foundationdb-rs/foundationdb-rs/issues/38
run_test --num-ops 1000 --api-version $fdb_api_version --test-name directory --concurrency 1 rust --no-directory-snapshot-ops --compare python --seed 4142326254
run_test --num-ops 1000 --api-version $fdb_api_version --test-name directory --concurrency 1 rust --no-directory-snapshot-ops --compare python --seed 4275610547
run_test --num-ops 1000 --api-version $fdb_api_version --test-name directory --concurrency 1 rust --no-directory-snapshot-ops --compare python --seed 1951034301
run_test --num-ops 1000 --api-version $fdb_api_version --test-name directory --concurrency 1 rust --no-directory-snapshot-ops --compare python --seed 1700644945

# https://github.com/foundationdb-rs/foundationdb-rs/issues/42
run_test --num-ops 1000 --api-version $fdb_api_version --test-name directory --concurrency 1 rust --no-directory-snapshot-ops --compare python --seed 584458794

# Database-level directory operations must retry concurrent commit conflicts.
run_test --num-ops 100 --api-version 610 --test-name directory_hca --concurrency 5 rust --seed 3179331380

seed_args() {
  if [ -n "${BINDINGTESTER_SEED:-}" ]; then
    echo "--seed $(( (BINDINGTESTER_SEED + 10 * $1 + $2) % 4294967296 ))"
  fi
}

START=1
END=${1-1}
for i in $(eval echo "{$START..$END}")
do
  echo "Running iteration $i"
  run_test --test-name scripted rust
  # shellcheck disable=SC2046
  run_test --num-ops 1000 --api-version $fdb_api_version --test-name api --compare python rust $(seed_args "$i" 1)
  # shellcheck disable=SC2046
  run_test --num-ops 1000 --api-version $fdb_api_version --test-name api --concurrency 5 rust $(seed_args "$i" 2)
  # shellcheck disable=SC2046
  run_test --num-ops 1000 --api-version $fdb_api_version --test-name directory --concurrency 1 rust --no-directory-snapshot-ops --compare python $(seed_args "$i" 3)
  # shellcheck disable=SC2046
  run_test --num-ops 100 --api-version $fdb_api_version --test-name directory_hca --concurrency 5 rust --no-directory-snapshot-ops $(seed_args "$i" 4)
done

summary
