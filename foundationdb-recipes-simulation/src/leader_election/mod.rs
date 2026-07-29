//! Leader election under the deterministic simulator.
//!
//! The workload drives the recipe's transaction-level primitives, records every
//! operation in a versionstamped log, and judges the run in the check phase.
//! The split between the two halves is deliberate:
//!
//! - [`log_schema`], [`replay`], [`invariants`] and [`swarm`] are pure. They
//!   know nothing about FoundationDB beyond the tuple layer, they take their
//!   inputs as values, and they are unit-tested against hand-mutated logs
//!   without a simulator anywhere in sight. Every invariant has a
//!   counterexample test that proves it can fail, and every property the
//!   configuration draw is supposed to have is asserted over ten thousand
//!   seeds.
//! - [`clock`], [`logged_op`], [`roles`] and [`workload`] are the machinery
//!   that produces those inputs from a real run.
//!
//! The reason for the split is the defect this rewrite exists to fix: the
//! previous suite had seven invariants that could not fail for any input, and
//! nothing in the build would have told anyone. A pure checker with mutation
//! tests cannot rot that way silently.
//!
//! # What the log is for
//!
//! FoundationDB gives us one thing no amount of client-side bookkeeping can:
//! a total order over commits. Each operation writes its record under an
//! incomplete versionstamp in the same transaction as the operation itself, so
//! the log is exactly the set of operations that committed, in the order they
//! committed. Replaying it reconstructs what the leader record must hold, and
//! comparing that against the record the database actually holds is the first
//! invariant.

mod clock;
pub mod invariants;
pub mod log_schema;
mod logged_op;
pub mod replay;
mod roles;
mod swarm;
mod workload;

pub use workload::LeaderElectionWorkload;
