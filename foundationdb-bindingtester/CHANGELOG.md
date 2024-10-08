# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.9.0] - 2024-10-08

### <!-- 0 -->🚀 added

- Bump fdb crate to 0.9.0 and sim to 0.2.0
- Use 7.3
- Get TenantManagement::get_tenant
- Implement list_tenant
- Implement Tenant{Create,SetActive,Delete}
- Add tenant operations
- Initial commit to support fdb 7.1
- Bump rust edition to 2021
- Bump crate version, maintainers and documentation
- Add warning on create
- Feat(foundationdb,bindingtester): Add directory
- Add support for api 630

### <!-- 1 -->🐛 Bug Fixes

- Clippy warnings
- Fix several panics during some seeds
- Fix(bindingTester): prevent overflow on i64::sub
- Cargo clippy --fix and fmt
- Fix(bindingtester): trim trailing \xff on strinc
- Target an existing structopt version
- Fix even `run` can lead to UB
- Simplify and fix UB in boot API
- Fix cargo clippy warning
- Fix bindingtester build with fdb610
- Fix versionstamp offset and bindingtester
- Try to fix bindingtester UnsupportedIntLength panic
- Fix TUPLE_PACK_WITH_VERSIONSTAMP typo
- Fix typo in bindingtester
- Fix bigint support
- Fix some cargo clippy issues
- Fix [#170](https://github.com/foundationdb-rs/foundationdb-rs/pull/170): protect boot from undefined behavior
- Fix Cargo.toml repository links
- Fix bindingtester failure (seed 144853353)
- Fix bindingtest api spurious test with seed 4186291629
- Fix bindingtester get_range limit
- Clippy fixes
- Fix bindingtester get_committed_version
- Fix some bugs in bindingtester
- Fix missing rename for fdb_api to api

### <!-- 2 -->🚜 Refactor

- Remove clikengo link in Cargo.toml
- Accept a slice instead of a Vec as path
- Bindingtester refactoring

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
- Handle error during GetApproximageSize
- Replace get(0) by first()
- Prepare release
- Hide tenant api behind a feature
- Allow running bindingTester on nixos
- Prepare for 0.7.0
- Merge pull request [#48](https://github.com/foundationdb-rs/foundationdb-rs/pull/48) from PierreZ/feat/7.0.0
- Chore(directory): cleanup tuple looking syntax
- Rollback breaking change on Subspace::from_bytes
- Chore: code-review
- Code-review
- Default to foundation 620 api
- Allow running bindingtester with different api versions
- Improve LogStack performance by 100x
- Few api improvements:
- :transact api is now generic and should be future proof.
- :transact data is now passed as mutable
- Foundationdb-sys version is now 0.4.0
- Transaction documentation and some API simplifications
- Cleanup dependencies and rename Error as FdbError
- Travis-ci
- Futures 0.3 is stable now
- Bindingtester is now complete and serde is replaced by a solution close to the original
- Working threaded bindingtester
- Foundationdb api 510, 520, 600 support
- Async/await: complete rewrite of foundationdb


