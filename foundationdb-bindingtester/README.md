Rust FoundationDB bindingtester
===============================

This exe implements the official FoundationDB bindingtester protocol.

By running `./bindingtester.py ${this_executable}`, you can test how the rust foundationdb bindings behave.

The following configurations are tested and should pass without any issue:

```
./bindingtester.py --test-name scripted
./bindingtester.py --num-ops 1000 --test-name api --api-version 610
./bindingtester.py --num-ops 1000 --concurrency 5 --test-name api --api-version 610
```

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
