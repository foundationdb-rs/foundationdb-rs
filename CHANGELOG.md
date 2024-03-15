# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.9.0] - 2024-03-15

### Bug Fixes

- **simulation**: Taking into account maybe_committed
- **foundationdb**: Make the db_run non capturing
- **foundationdb**: Apply cargo fmt recommendation
- **foundationdb**: Fix std::slice::from_raw_parts behavior on nightly
- **all**: Add more feature flag
- **foundationdb**: Remove flappy tests
- **all**: CI
- **CI**: Include the right features upon doc generation
- **simulation**: Use idempotent txn as an example
- **simulation**: Use retry loop in example

### Features

- **ci**: Refactor how tests are runned
- **sim**: Take into account maybe_committed txn in the example
- **ci**: Add cargo audit on MR and weekly basis
- **macro**: Add support for 7.3
- **sys**: Add support for 7.3
- **fdb**: Add support for 7.3
- **bindingtester**: Use 7.3
- **foundationdb**: Add Database.get_client_status
- **fdb**: Add support for tenant info in 7.3
- **simulation**: Initial commit to handle multiple versions
- **simulation**: Bump CI and example to use 7.3
- **simulation**: Build atomic example with Docker
- **simulation**: Rework build.rs, doc and multi-version compilation
- **all**: Bump fdb crate to 0.9.0 and sim to 0.2.0

### Miscellaneous Tasks

- **all**: Bump msrv
- **all**: Clippy: Simplify non-minimal-cfg.
- **all**: Using `subsec_millis` is more cooncise.
- **all**: Stop using most actions-rs actions.
- **all**: Initial setup for CI(simulation)
- **all**: Bump flake
- **simulation**: Explicit drop on clippy
- **all**: Bump flake
- **all**: Bump MSRV
- **all**: Bump MSRV
- **all**: Bump dependencies
- **ci**: Remove unecessary binaries to release disk space
- **all**: Nix flake update
- **gen**: Add support for 730
- **all**: Bump fdb version
- **all**: Cargo fmt
- **all**: Bump flake
- **foundationdb**: Removing default fdb version
- **all**: Specify fdb's version during CI
- **all**: Cargo fmt/clippy
- **all**: Replace get(0) by first()
- **all**: Bump flake
- **all**: Link against libc
- **bindingtester**: Handle error during GetApproximageSize
- **ci**: Add coverage for tenants
- **all**: Bump dep
- **all**: Generate changelog with git-cliff

### Build

- **all**: Depend stopwatch in sources

### Clippy

- **all**: Fix needless return warnings.
- **all**: Remove unnecessary casts.
- **all**: Derive `Default` for `FdbOptionTy`.
- **all**: Suppress "should_implement_trait" for `Database::default()`.

## [0.8.0] - 2023-06-07

### Bug Fixes

- **all**: Clippy warnings
- **ci**: Enable tenant on every GH action
- **foundationdb**: Run tenants tests when feature is used
- **foundationdb**: Avoid infinite recursion on debug impl
- **all**: Mark Fdb struct wrappers as packed
- **foundationdb**: Use Debug impl as Display

### Documentation

- **all**: Add 0.7.0 change-log

### Features

- **all**: Add more useful version of get_mapped_ranges
- **foundationdb**: Add perform_no_op method
- **foundationdb**: Introduce db_run
- **foundationdb**: Introduce FdbBindingError
- **foundationdb**: Remove panic during RetryableTransaction::commit
- **foundationdb**: Rework RetryableTransaction
- **foundationdb**: Add raw method to create a Database
- **foundationdb**: Initial commit for Tenant
- **bindingtester**: Add tenant operations
- **bindingtester**: Implement Tenant{Create,SetActive,Delete}
- **foundationdb**: Implement list_tenant
- **foundationdb**: Get TenantManagement::get_tenant
- **foundationdb**: Fix `list_range` and add tenant::run
- **ci**: Run bindingTester on different rust toolchain

### Miscellaneous Tasks

- **all**: Initial support for Nix
- **all**: Bump rust to 1.57
- **all**: Fix table and rust-version badge in README
- **all**: Build on docs.rs with the required features
- **all**: Add cargo-edit to nix env
- **foundationdb**: Add hello_world using db.run
- **all**: Reformat table
- **foundationdb**: Use cargo add in doc
- **all**: Clippy
- **ci**: Rename examples
- **all**: Bump flake
- **ci**: Bump actions/checkout
- **all**: Allow running bindingTester on nixos
- **ci**: Enable tenant
- **foundationdb**: Remove unused StreamingMode
- **ci**: Bump fdb
- **all**: Nix flake update
- **foundationdb**: Hide tenant api behind a feature
- **ci**: Allow workflow dispatch
- **all**: Bump msrv
- **all**: Bump flake
- **all**: Cargo {fmt,clippy}
- **all**: Bump flake
- **all**: Prepare release
- **all**: Bump deps
- **all**: Bump msrv for bindgen and env_logger
- **foundationdb-sys**: Bump dependencies

## [0.7.0] - 2022-07-11

### Bug Fixes

- **all**: Nighly build by removing layout tests
- **all**: Use right branch for bindingTester
- **foundationdb-macros**: Avoid using the macros on the docs

### Features

- **foundationdb**: Add micro index example
- **all**: Add support for api 700
- **all**: Initial commit for get_range_split_points
- **foundationdb**: Implement Iterator for FdbKeys
- **foundationdb**: Expose FdbKey and other only in 7.0
- **bindingTester**: Implement range split point
- **all**: Introduce new crate to hold macros
- **foundationdb**: Introduce get/set metatadaVersion
- **foundationdb**: Introduce run method
- **tuple**: Add Hash, Eq and PartialEq to Subspace
- **all**: Initial commit to support fdb 7.1
- **foundationdb**: Initial commit for get_mapped_range
- **foundationdb**: Allow retrieval of embedded kvs in mapped_range
- **foundationdb**: Allow KeySelector in mapped_range
- **foundationdb**: Improve doc about mapped_range

### Miscellaneous Tasks

- **all**: Bump dependencies
- **all**: Enable example simple-index and blob
- **all**: Remove clikengo link in Cargo.toml
- **foundationdb**: Reduce use of `cfg_api_versions`
- **foundationdb**: Split modules in separate files
- **foundationdb**: Move test mapped_range into range's tests
- **foundationdb**: Improve documentation
- **all**: Bump deps
- **all**: Bump rust patch version
- **all**: Prepare for 0.7.0

### Refactor

- **foundationdb**: Use macro cfg_api_versions when possible

### Testing

- **all**: Add tested example to explain how to use fdb to store blob data.
- **all**: Improve blob example display
- **all**: Use tokio reactor in examples
- **all**: Improve blob example with a manifest system
- **all**: Improve blob example display
- **all**: Add missing async keyword on simple-index example
- **all**: Add explanation about blob example
- **all**: Add a simple blob example

## [0.6.0] - 2022-04-08

### Bug Fixes

- **ci**: Use python2 for bindingtester
- **ci**: Target an existing structopt version
- **all**: Bump minimum version to rust 1.46
- **bindingtester**: Trim trailing \xff on strinc
- **foundationdb-gen**: Include fdb.options for 630
- **foundationdb**: Run estimate range tests only on 630
- **bindingTester**: Prevent overflow on i64::sub
- **all**: Discord invitation link
- **bindingTester**: Fix several panics during some seeds
- **foundationdb**: Typo
- **directory**: Avoid throwing bad CannotMoveBetweenPartition
- **directory**: Avoid initializing the version key on create_or_open
- **directory**: Scan old and new path during Directory.Move

### Features

- **all**: Add support for api 630
- **all**: Introduce platform tier
- **foundationdb,bindingtester**: Add directory
- **all**: Bump crate version, maintainers and documentation
- **all**: Bump rust edition to 2021
- **all**: Adding deps.rs badge

### Miscellaneous Tasks

- **all**: Try to use latest version of foundationdb-actions-install
- **all**: Simplify env
- **all**: Disable running doc test on code coverage
- **all**: Fix typo
- **all**: Stronger settings for running bindingtester
- **all**: Force binding tester to compare against python
- **all**: Remove windows
- **all**: Remove old CI, fix links
- **ci**: Upgrade gh action to deploy doc
- **ci**: Add Rust beta and nightly
- **ci**: Trigger CI on push only from master/main branch
- **all**: Removing reference to master branch
- **ci**: Run the bindingTester every hour on Github actions
- **ci**: Reduce number of iteration for automated bindingTester
- **all**: Cargo clippy --fix and fmt
- **all**: Bump gh action version
- **bindingTester**: Install requirements
- **community**: Add Discord link
- **ci**: Disable scheduled workflows on forks
- **ci**: Run more interation of the BindingTester for coverage
- **all**: Code-review
- **all**: Fix clippy warnings
- **directory**: Add warning on create
- **all**: Fix compilation with 1.46.0 and add back install on script
- **all**: Code-review
- **all**: Code-review
- **directory**: Rollback breaking change on Subspace::from_bytes
- **directory**: Cleanup tuple looking syntax
- **all**: Update CHANGELOG.md
- **all**: Fix code in documentation
- **all**: Bump deps
- **all**: Clippy

### Refactor

- **directory**: Accept a slice instead of a Vec as path
- **directory**: Node are always loaded and existing

### Bindingtester

- **all**: Improve LogStack performance by 100x

## [0.4.0] - 2020-01-23

### Database

- **all**: :transact data is now passed as mutable
- **all**: :transact api is now generic and should be future proof.

### Miscellaneous Tasks

- **all**: Auto-generate all code at build time
- **all**: Rustdoc should now work
- **all**: Fix windows fdb status
- **all**: Use foundationdb-actions-install
- **all**: On pull_request is not required when push is provided
- **all**: Fix foundationdb actions install version
- **all**: Remove windows from travis-ci
- **all**: Show FoundationDB status for all os
- **all**: Run bindingtester for codecoverage
- **all**: Force bindingtester build

### README

- **all**: Update links and badges

### Async/await

- **all**: Complete rewrite of foundationdb

### Fdb

- **all**: Foundationdb 5.2.5 -> 6.0.15

### Travis

- **all**: Move to ubuntu xenial (16.04 LTS)

### Travis/appveyor

- **all**: Move CI to FoundationDB 5.2.5

### Tuple

- **all**: Fix panic on i64
- **all**: Handle when encoded value overflows

## [0.3.0] - 2018-11-16

### Testing

- **all**: Add testcases with conflict scenario

### Bench

- **all**: Add support for threading/batching

### Examples/hello

- **all**: Remove unsafe from futures
- **all**: Add `example_get_multi`
- **all**: Default config path based on OS
- **all**: Use defualt path for all examples

### Gen

- **all**: Use consts from sys crate

### Scripts

- **all**: Add support for rhel/centos

### Transact

- **all**: Allow custom error type

### Travis

- **all**: Pull rustfmt from rustup, #48

### Trx

- **all**: Change `TrxCommit::Item` to `Transaction`
- **all**: Cancel/committed_version/get_address_for_key
- **all**: Versionstamp, {get,set}_read_version, #20

### Tuple

- **all**: Add floating-point support
- **all**: Integer encoding
- **all**: Fix crash with uninitialized memory access
- **all**: Support tuple encoding
- **all**: Fix bug on binary decoding
- **all**: Fix off-by-one on integer encoding
- **all**: Support nested Empty, fix overflow on int

[0.9.0]: https://github.com///compare/v0.8.0..v0.9.0
[0.8.0]: https://github.com///compare/v0.7.0..v0.8.0
[0.7.0]: https://github.com///compare/v0.6.0..v0.7.0
[0.6.0]: https://github.com///compare/v0.4.1..v0.6.0
[0.4.0]: https://github.com///compare/v0.3.0..v0.4.0

<!-- generated by git-cliff -->
