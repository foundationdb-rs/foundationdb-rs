# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.4.0](https://github.com/foundationdb-rs/foundationdb-rs/compare/foundationdb-simulation-v0.3.0...foundationdb-simulation-v0.4.0) - 2026-11-28

### <!-- 0 -->⚠️ BREAKING CHANGES

- Borrow metric sinks for their callback lifetime
- Require unsafe process handle selection
- Require unsafe raw future construction
- Validate metric formats before native formatting

### <!-- 1 -->🚀 Features

- Feat(simulation): implement Clone for WorkloadContext
- Feat(simulation): simulator-backed SimClock, SimRng and WorkloadContext::environment
- Feat(simulation): tag Rust workload trace events for failure triage
- Collect LLVM coverage from external workloads

### <!-- 2 -->🐛 Bug Fixes

- Fix(simulation): drop let-chain in fdb_rt for MSRV, pin toolchain
- Fix(simulation): use c_char instead of i8 at FFI boundaries
- Fix(simulation): draw next_u32 straight from the simulator generator
- Escape NUL bytes in trace strings
- Keep futures on their owning executor thread
- Acquire coalesced wake publication before polling

### <!-- 4 -->🚜 Refactor

- Refactor(simulation): sim wall time counts from the UNIX epoch
- Refactor(simulation): fold the budget proof into the atomic example

### <!-- 5 -->📚 Documentation

- Docs: warn about RandomState non-determinism and directory concurrency
- Docs(simulation): build the example workloads around env-injected structs
- Docs(simulation): add budget example proving time budgets under the simulator
- Docs(simulation): document how WorkloadContext::delay can exhaust memory

### <!-- 6 -->🧪 Testing

- Focus metric coverage on callback lifetime
- Consolidate executor safety coverage

### <!-- 8 -->⚙️ Miscellaneous

- Style: collapse nested ifs into let chains



## [0.3.0] - 2026-06-24

### <!-- 1 -->🚀 Features

- Feat(simulation): auto-capitalize trace detail names
- Feat(simulation): WorkloadContext::delay takes Duration, exercised in noop
- Feat: migrate workspace to Rust 2024 edition (#255)

### <!-- 2 -->🐛 Bug Fixes

- Fix(simulation): static storage for C-ABI vtables, target fdbserver 7.4.6

### <!-- 9 -->📦 Other Changes

- Rewrite fdb_rt to split wakeup and polling phase

[0.3.0]: https://github.com/foundationdb-rs}/foundationdb-rs/compare/0.2.3..0.3.0


## [0.2.3] - 2025-10-24

### <!-- 1 -->🚀 Features

- Add support 7.4
- Add 7.4 simulation in CI

### <!-- 2 -->🐛 Bug Fixes

- Remove unwrap on checks
- Recover simulation trace public api breaking change
- Fix(foundationdb-simulation): add docs.rs metadata to fix build

### <!-- 4 -->🚜 Refactor

- Remove unnecessary simulation headers

### <!-- 8 -->⚙️ Miscellaneous

- Clippy fixes

### <!-- 9 -->📦 Other Changes

- Update CI and bindingtester to 7.4
- Some spaces and tabs doc formalization
- Update workload bindings
- Update README
- Async RustWorkload
- Tidy up foundationdb-simulation API:

[0.2.3]: https://github.com/foundationdb-rs}/foundationdb-rs/compare/0.2.2..0.2.3


## [0.2.2] - 2025-01-06

### <!-- 0 -->🚀 added

- Add unit for now()

### <!-- 3 -->🆙 Bump

- Bump cc from 1.1.30 to 1.2.1
- Bump cc from 1.1.24 to 1.1.30

[0.2.2]: https://github.com/foundationdb-rs}/foundationdb-rs/compare/0.2.1..0.2.2

## [0.2.1] - 2024-10-08

### <!-- 1 -->🐛 Bug Fixes

- Fix foundationdb-simulation compilation with fdb-7_1

### <!-- 3 -->🆙 Bump

- Bump all dependencies and setup dependabot

### <!-- 4 -->⚙️ Other changes

- Use C++14 spec

[0.2.1]: https://github.com/foundationdb-rs}/foundationdb-rs/compare/0.2.0..0.2.1

