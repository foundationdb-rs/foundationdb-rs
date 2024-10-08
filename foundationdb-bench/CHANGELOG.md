# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.5.1] - 2024-10-08

### <!-- 0 -->🚀 added

- Bump rust edition to 2021
- Add support for threading/batching
- Add benchmark

### <!-- 1 -->🐛 Bug Fixes

- Fix needless return warnings.
- Target an existing structopt version
- Fix even `run` can lead to UB
- Simplify and fix UB in boot API
- Fix cargo clippy warning
- Fix [#170](https://github.com/foundationdb-rs/foundationdb-rs/pull/170): protect boot from undefined behavior
- Fix Cargo.toml repository links
- Clippy fixes

### <!-- 2 -->🚜 Refactor

- Remove clikengo link in Cargo.toml
- Remove `Send` from `FdbFuture`

### <!-- 3 -->🆙 Bump

- Bump MSRV to 1.71.1
- Bump all dependencies and setup dependabot
- Bump dep
- Bump dependencies
- Bump MSRV
- Bump msrv for bindgen and env_logger
- Bump deps
- Bump crates version
- Chore: bump rust to 1.57
- Bump version to 5.0.1
- Bump version to 0.4.1

### <!-- 4 -->⚙️ Other changes

- Build(deps): update env_logger requirement from 0.10.2 to 0.11.5
- Using `subsec_millis` is more cooncise.
- Fmt and clippy
- Build: depend stopwatch in sources
- Documentation and API improvements
- Cleanup dependencies and rename Error as FdbError
- Futures 0.3 is stable now
- Working threaded bindingtester
- Foundationdb api 510, 520, 600 support
- Async/await: complete rewrite of foundationdb
- Rust edition 2018
- Upgrade rand from 0.4 to 0.6 ([#104](https://github.com/foundationdb-rs/foundationdb-rs/pull/104))
- Update env_logger requirement from 0.5 to 0.6 ([#100](https://github.com/foundationdb-rs/foundationdb-rs/pull/100))
- Travis on windows
- Prepare 0.3 release
- Update changelog for 0.2
- Allow the association of other errors to FdbError


