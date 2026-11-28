# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.12.0](https://github.com/foundationdb-rs/foundationdb-rs/compare/foundationdb-tuple-v0.11.0...foundationdb-tuple-v0.12.0) - 2026-11-28

### <!-- 0 -->⚠️ BREAKING CHANGES

- Reject incompatible legacy versionstamp mutations
- Chore!: bump MSRV to 1.89.0

### <!-- 2 -->🐛 Bug Fixes

- Encode legacy versionstamp keys without restricting raw mutations

### <!-- 6 -->🧪 Testing

- Test(tuple): add typecode-band ordering test



## [0.11.0] - 2026-06-24

### <!-- 1 -->🚀 Features

- Feat: migrate workspace to Rust 2024 edition (#255)

[0.11.0]: https://github.com/foundationdb-rs}/foundationdb-rs/compare/0.10.0..0.11.0


## [0.10.0] - 2025-10-24

### <!-- 1 -->🚀 Features

- Implement `std::fmt::Display` on `Subspace`

### <!-- 2 -->🐛 Bug Fixes

- Fix: resolve clippy warnings and add doc alias

### <!-- 8 -->⚙️ Miscellaneous

- Clippy fixes
- Bump MSRV to 1.82
- Clippy --fix
- Bump msrv to 1.85

[0.10.0]: https://github.com/foundationdb-rs}/foundationdb-rs/compare/0.9.1..0.10.0


## [0.9.1] - 2025-01-06

### <!-- 4 -->⚙️ Other changes

- Update release
- Split the fdb-tuple module into its own crate


