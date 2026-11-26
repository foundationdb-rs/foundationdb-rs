Rust FoundationDB bindingtester
===============================

This binary implements the official FoundationDB bindingtester stack machine protocol, so the
Rust bindings can be checked against the specification and compared with the Python bindings.

It is invoked by the harness as `bindingtester PREFIX API_VERSION [CLUSTER_FILE]`. You normally
do not run it by hand: set it up once and use the run script, which needs a running cluster.

```bash
cargo build -p bindingtester
./scripts/setup_bindingtester.sh target/debug/bindingtester   # one-time
./scripts/run_bindingtester.sh 10                              # N iterations
```

Each iteration runs the `scripted`, `api` (with and without `--concurrency 5`), `directory`
and `directory_hca` tests at the latest API version, after a fixed list of regression seeds.
`--compare python` is used wherever the harness supports it.

## Code layout

- `main.rs`: the stack machine loop and the non-directory instructions.
- `instr.rs`: instruction parsing (`Instr`, `InstrCode`).
- `stack.rs`: stack items and the futures pushed on the stack.
- `trx_state.rs`: the per-name transaction state.
- `directory.rs`: the directory layer instructions.
- `unit_tests.rs`: the `UNIT_TESTS` instruction, a port of the Python bindings' unit tests.

## Debugging a failing seed

`scripts/run_bindingtester.sh` does not stop at the first failure. For each failed run it
prints a command that reproduces it, and a summary of all of them at the end. Every run has
an explicit `--seed`, even the random ones.

1. Rerun the failing command with full logs and traces. Set `BINDINGTESTER_TRACE` to a
   directory: each run then writes the harness and tester output (with `RUST_LOG=debug` for
   the Rust tester unless `RUST_LOG` is set) and the FDB client trace files of both
   processes into its own subdirectory. Subdirectories of passing runs are deleted.

   ```bash
   BINDINGTESTER_TRACE=target/bt-logs ./scripts/run_bindingtester.sh 1
   ```

   The Rust tester tags each of its transactions with `DebugTransactionIdentifier("RUST")`
   and `LogTransaction`, so they can be found in the traces.

2. Shrink the failure to its minimal number of operations, and print its instructions, by
   passing the same arguments to `scripts/bisect_bindingtester.sh`:

   ```bash
   ./scripts/bisect_bindingtester.sh --num-ops 1000 --api-version 740 \
     --test-name api --compare python rust --seed 3534790651
   ```

   The harness cannot bisect concurrent tests (`--concurrency` greater than 1).

3. In CI, the correctness jobs run with `BINDINGTESTER_TRACE` set and upload the logs of the
   failing runs as the `bindingtester-logs-*` artifacts of the workflow run.

Other knobs of `scripts/run_bindingtester.sh`: `BINDINGTESTER_SEED` makes the random loop
reproducible from a base seed (the coverage job uses it), and `BINDINGTESTER_MAX_FAILURES`
(default 10) stops the run after that many failures.
