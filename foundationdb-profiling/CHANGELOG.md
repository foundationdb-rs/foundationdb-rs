# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0](https://github.com/foundationdb-rs/foundationdb-rs/releases/tag/foundationdb-profiling-v0.1.0) - 2026-11-28

### <!-- 0 -->⚠️ BREAKING CHANGES

- Feat(foundationdb-profiling)!: embeddable reader for client profiling data
- Feat(foundationdb-profiling)!: ProfileScanner bounds every page with its own budget
- Refactor(foundationdb-profiling)!: ClientBudget is the only scan bound
- Refactor(foundationdb-profiling)!: drop the foundationdb dependency
- Chore!: bump MSRV to 1.89.0

### <!-- 1 -->🚀 Features

- Profiling Keyspace Scanner
- Add support 7.4
- Feat: migrate workspace to Rust 2024 edition (#255)
- Feat(foundationdb-profiling): add ScanError::into_error

### <!-- 2 -->🐛 Bug Fixes

- Fix: satisfy MSRV clippy (precedence, needless_lifetimes)
- Fix(foundationdb-profiling): address review findings

### <!-- 5 -->📚 Documentation

- Doc-related clippy fixes
- Docs(foundationdb-profiling): add a top_keys example

### <!-- 7 -->🔧 Build

- Ci: run foundationdb-profiling in a dedicated workflow with 100% sampling

### <!-- 8 -->⚙️ Miscellaneous

- List Pierre Zemb as the only author
- Chore(foundationdb-profiling): align Cargo.toml with the other crates
- Chore(foundationdb-profiling): prepare the crate for publishing
- Style: collapse nested ifs into let chains


