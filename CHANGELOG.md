# Change Log: foundationdb-rs

All notable changes to this project will be documented in this file.
This project adheres to [Semantic Versioning](http://semver.org/).

## 0.7.0

- #62: Add Hash, Eq and PartialEq to Subspace
- #61: Add and defaults to FoundationDB 7.1.X API version
- #55: Add metadataVersion on transaction
- #48: Add and defaults to FoundationDB 7.0.X API version
- #37: Bump dependencies

## 0.6.0

- #34: Bump dependencies
- #33: Bump MSRV to Rust 2021 (1.56.0)
- #26: Add Directory
- #19: Add and defaults to foundationdb 6.3.x API version
- #15: Introduce Platform tier
- #11: Move from master branch to main
- Moved repo from Clikengo to foundationdb-rs

## 0.5.1

- #207: Better documentation of API init methods

## 0.5.0

- #203: Defaults to foundationdb 6.2.x API version
- #194: Add support for pack with versionstamp
- #194: Fix a bug in the unpacking of signed integers
- #194: Add support for BigInt on Element
- #194: Element now implement Ord and matches the packed ordering
- #194: Bindingtester output now matches official python output
- #179, #182, #202, #204: Fix possible runloop undefined behaviors
  (`fdb_stop_network` **MUST** be called before the program exits, see issues #170, #181, #202).
  Fixing it required a breaking change with the `foundationdb::boot()` API
- #177: Add support for NEGINTSTART, POSINTEND encoding (@garrensmith)
- #178: Add support for `num-bigint`
- #184: Fix use after free in `Database::new`, `Cluster::new`
- #187: Add `#[non_exhaustive]` on generated enums

## 0.4.2

- #183: Fix use after free in `Database::new`, `Cluster::new`

## 0.4.1

- Fix docs.rs build issues

## 0.4.0

- Migration to stable (rust 1.40+) async/await
- Transaction aren't cloned anymore, they are shared by reference. Commit/cancel/reset api requires owned/mutable access to a Transaction. This protect against undefined behavior that was previously possible (cancel/reset) data races.
- No more indirection within FdbFuture. Returned future give you direct access to the result.
- Support for fdb api 610+
- Option generation is now indented and the code is simpler
- RangeOption and KeySelector can be either be Owned or Borrowed
- KeySelector offset can be negative (there is a test of this in the binding checker, this was not found due to casting luck)
- Some int options can be negative
- Fix init api safety (undefined behavior was possible)
- Simple boot process
- Foundationdb 510, 520, 600 support with common Database::new_compat api
- Threaded bindingtester (concurrent scripted and api tests)

## 0.3.0

### Changed

- `GetKeyResult` and `GetAddressResult` return value no longer unwrap to Result #94 (@yjh0502)

### Added

- Win64 support #92 (@Speedy37)

## 0.2.0

### Added

- Database::transact (#34, @yjh0502)
- RangeOptionBuilder::from_tuple (#81, @rushmorem)
- Subspace (#54 #56 #57 #76 #78, @yjh0502 @rushmorem)
- Transaction::watch (#25 #59, @yjh0502)
- Transaction::atomic_op (#26, @yjh0502)
- Transaction::get_range (#28, @yjh0502)
- Transaction::{get, set}\_read_version (#38, @yjh0502)
- Transaction::add_conflict_range (#50, @yjh0502)
- Tuple interfaces (#40 #41 #42 #46 #47 #51 #60 #62 #64 #67 #74 #80 #83, @yjh0502 @rushmorem)
- Additional tests for Transactions (#33, @yjh0502)
- Class Scheduling Tutorial in examples (#65)
- FoundationDB bindingtester support (#39 #43 #45, @yjh0502)
- FoundationDB benchmarking test suite support (#70 #73, @yjh0502)
- Support to scripts for installing on Centos/RHEL

### Changed

- Added TupleError to foundationdb::Error (#77)
- API names more inline with Rust style guidelinse (#84 @rushmorem)

## 0.1.0

### Added

- first release
- C api Bindings
- Cluster API
- Database API
- Network API
- Transaction API
- Options generation
- FdbFuture abstraction over Futures 0.1 API
- fdb_api initialization
- FdbError conversion
