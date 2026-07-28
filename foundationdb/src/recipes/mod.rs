//! # FoundationDB Recipes
//!
//! This module provides high-level distributed system recipes for FoundationDB,
//! similar to Apache Curator for ZooKeeper. These recipes implement common
//! distributed system patterns and primitives on top of FoundationDB's
//! transactional key-value store.
//!
//! ## Available Recipes
//!
//! - **Leader Election** (`leader_election`): one contested record decides who
//!   leads, for a term identified by a monotonic ballot. A contender takes a
//!   term from a live holder only after watching the record hold still for a
//!   full lease on its own monotonic clock, so no wall clock is ever compared
//!   across processes. Ships both the transaction-level primitives and an async
//!   handle that campaigns, renews and hands the term back around your work.
//! - **Ranked Register** (`ranked_register`): the ranked register of Chockler
//!   and Malkhi's Active Disk Paxos, a value guarded by monotonic ranks that
//!   rejects writes from anybody a higher rank has fenced out.
//!
//! The two compose into an "election service": the election decides who may
//! act, and the register is what stops anybody else from acting. Winning a term
//! fences nothing by itself, so a new leader installs its fence with
//! `RankedRegister::read` at its own rank before doing any fenced work. Both
//! module documentations describe the contract; neither recipe requires the
//! other.
//!
//! ## Usage
//!
//! Each recipe is behind its own feature flag to keep the core library lightweight.
//! Enable the recipes you need in your `Cargo.toml`:
//!
//! ```toml
//! [dependencies]
//! foundationdb = { version = "*", features = ["recipes-leader-election"] }
//! ```
//!
//! Or enable all recipes at once:
//!
//! ```toml
//! [dependencies]
//! foundationdb = { version = "*", features = ["recipes"] }
//! ```

// Both modules carry their own documentation. Deliberately no `///` here: a
// doc comment on the declaration is merged with the module's own, and rustdoc
// then resolves the whole merged text in *this* module's scope, breaking every
// intra-doc link written inside the module.
#[cfg(feature = "recipes-leader-election")]
pub mod leader_election;

#[cfg(feature = "recipes-ranked-register")]
pub mod ranked_register;
