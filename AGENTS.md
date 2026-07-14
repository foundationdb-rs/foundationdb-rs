# AGENTS.md

This file provides guidance to coding agents (Claude Code, etc.) when working with code in
this repository. Claude Code loads it via `@AGENTS.md` from `CLAUDE.md`.

## What this is

`foundationdb-rs` is a futures-based Rust client for FoundationDB layered over the FDB C API.
It is a Cargo workspace (resolver 2, Rust edition 2024, MSRV 1.85.1) supporting many FDB API
versions (5.1 through 7.4) from a single codebase via cargo features.

## Build / test / lint

Use the cargo aliases in `.cargo/config.toml` rather than spelling out feature flags. They
target the latest FDB (7.4) with embedded headers, so they build without FoundationDB
installed (works under Nix):

```bash
cargo build-fdb-latest     # build -p foundationdb --features embedded-fdb-include,fdb-7_4
cargo test-fdb-latest      # test  ''            ''
cargo check-fdb-latest     # check ''            ''
cargo clippy-fdb-latest    # clippy ''           ''
cargo doc-fdb-latest       # doc   ''            ''
cargo clippy-all           # whole-workspace clippy, all features, -D warnings (run before pushing)
```

Always run `cargo fmt --all` and `cargo clippy-all` before committing (CI gates on both;
`cargo clippy-all` is exactly what CI runs).

Other crates:
```bash
cargo test -p foundationdb-tuple --all-features
cargo test -p foundationdb-macros
```

Run a single test: `cargo test-fdb-latest <test_name>` (append filters after `--`). Most
`foundationdb` integration tests need a reachable FDB cluster.

### Running tests against a real cluster

Unit tests in `foundationdb` talk to a live cluster. Start one with Docker:
```bash
docker run -p 4500:4500 --name fdb --rm -d foundationdb/foundationdb:7.4.3
docker exec fdb fdbcli --exec "configure new single memory"
```
The cluster file must point at it (e.g. `/etc/foundationdb/fdb.cluster`).

### Nix

`nix develop` provides the toolchain, clang/bindgen, `fdbserver74`/`libfdb74`, the Python
venv for the bindingtester, and release tooling. Env vars (LIBCLANG_PATH, FDB_CLIENT_LIB_PATH,
etc.) are set by `flake.nix`.

## Feature flags (API version selection)

`foundationdb`, `foundationdb-sys`, and `foundationdb-gen` are gated by exactly one FDB
version feature: `fdb-5_1`, `fdb-5_2`, `fdb-6_0`..`fdb-6_3`, `fdb-7_0`, `fdb-7_1`, `fdb-7_3`,
`fdb-7_4`. Other useful features: `embedded-fdb-include` (compile without FDB installed),
`uuid` + `recipes` (defaults of `foundationdb`), `num-bigint`, `trace`.

## Architecture

Layered, bottom to top:

- **foundationdb-sys** - raw `bindgen` FFI to `libfdb_c`. Embedded headers live in
  `foundationdb-sys/include/<version>/`; `build.rs` picks the dir from the version feature.
  Defines the `if_cfg_api_versions!` macro for combinatorial version gating.
- **foundationdb-gen** - parses `fdb.options` XML and emits type-safe option enums
  (`DatabaseOption`/`TransactionOption`/`NetworkOption`) with `code()` + unsafe `apply()`.
  Invoked from `foundationdb/build.rs` at build time; output lands in `OUT_DIR/options.rs`.
- **foundationdb-macros** - `#[cfg_api_versions(min=.., max=..)]` proc macro expanding to the
  right per-version cfg flags.
- **foundationdb-tuple** - the FDB tuple layer (pack/unpack with type codes), `Subspace`,
  `Versionstamp`. Re-exported from the main crate.
- **foundationdb** - the public async API. Key files in `foundationdb/src/`:
  - `api.rs` - process-global `NetworkLifecycle` state machine managing the C API and the
    network event-loop: safe idempotent `boot()`, lazy start from `Database` constructors,
    automatic stop + join at process exit (atexit hook), explicit `stop_network()`.
  - `database.rs` - `Database`, plus the closure retry loop `run()` / `instrumented_run()`
    with backoff and the pluggable `RunnerHooks` trait (no-op vs metrics paths).
  - `transaction.rs` - `Transaction` (get/set/clear/atomic_op), commit/retry error types.
  - `future.rs` - `FdbFuture<T>`: non-blocking poll of `FDBFuture` + waker callback, the
    bridge to async/await across the network thread.
  - `directory/` - `Directory` trait + `DirectoryLayer` (hierarchical tuple-prefixed paths).
  - `keyselector.rs`, `tuple.rs`, `error.rs` (`FdbError`/`FdbBindingError`), `metrics.rs`,
    `recipes/`.

What is generated vs hand-written: options and their apply methods are **generated**; retry
loop, transaction lifecycle, directory/tuple layers are **hand-written**.

### Other workspace crates

- **foundationdb-bindingtester** - implements Apple's official bindingtester protocol; how we
  prove spec compliance against the Python bindings.
- **foundationdb-simulation** - load Rust workloads (`RustWorkload` trait) into FDB's
  deterministic simulator as a cdylib. NOTE: the C-API path needs `fdbserver` >= 7.4.6
  (7.4.3/.4/.5 have an incompatible ABI); 7.1/7.3 use the C++ shim via the official Docker image.
- **foundationdb-recipes-simulation** - example simulation workloads (leader election, etc.).
- **foundationdb-profiling** - async helpers to fetch transaction profiling data.

## Correctness testing (bindingtester)

The bindingtester runs our implementation against the official spec and compares with the
Python bindings. It needs a running cluster and the Python venv:
```bash
cargo build -p bindingtester
./scripts/setup_bindingtester.sh target/debug/bindingtester   # one-time
./scripts/run_bindingtester.sh 1000                            # N iterations
```
CI runs this hourly and on PRs labelled `correctness`.

## Conventions

- Commit messages follow Conventional Commits (`feat`, `fix`, `docs`, `chore`, ...).
- `CHANGELOG.md` files are generated by release-plz / git-cliff. Do not hand-edit them.
- Code is multi-version: when adding version-specific code, gate it with
  `if_cfg_api_versions!` (sys) or `#[cfg_api_versions]` (macros) instead of bare `#[cfg]`.
- `boot()` is safe and idempotent; tests can boot (or just create a `Database`) in any order,
  in parallel. Never call `api::stop_network()` in tests except the dedicated lifecycle test
  (`foundationdb/tests/api.rs`): it is terminal for the process.
