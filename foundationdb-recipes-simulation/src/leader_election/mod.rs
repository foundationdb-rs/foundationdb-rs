//! Leader election under the deterministic simulator.
//!
//! The workload drives the recipe's transaction-level primitives, records every
//! operation in a versionstamped log, and judges the run in the check phase.
//! The split between the two halves is deliberate:
//!
//! - [`log_schema`], [`replay`], [`invariants`], [`elector_invariants`] and
//!   [`swarm`] are pure. They know nothing about FoundationDB beyond the tuple
//!   layer, they take their
//!   inputs as values, and they are unit-tested against hand-mutated logs
//!   without a simulator anywhere in sight. Every invariant has a
//!   counterexample test that proves it can fail, and every property the
//!   configuration draw is supposed to have is asserted over ten thousand
//!   seeds.
//! - [`clock`], [`logged_op`], [`roles`], [`timer`], [`elector_role`] and
//!   [`workload`] are the machinery that produces those inputs from a real run.
//!
//! The reason for the split is the defect this rewrite exists to fix: the
//! previous suite had seven invariants that could not fail for any input, and
//! nothing in the build would have told anyone. A pure checker with mutation
//! tests cannot rot that way silently.
//!
//! # Every loop is bounded
//!
//! Nothing here waits on a notification. Every role polls: it acts, it parks on
//! a delay, it goes round again, and the run ends when simulated time reaches
//! the deadline. That only terminates if the parks really happen, so a failed
//! wait is never treated as a wait that succeeded. Four layers say so, in
//! decreasing order of how structural they are:
//!
//! - loops pace on an absolute cursor that moves *before* the wait
//!   ([`liveness::next_tick`]), so a wait that returns instantly still leaves
//!   the next round asking for a real one. It is FoundationDB's own
//!   `delayUntil` idiom and it holds without anybody checking anything;
//! - a delay that errors ends the role that asked for it, which is what the
//!   simulator means by that error;
//! - [`liveness`] stops a loop that has gone round three times without
//!   simulated time moving, whatever the cause;
//! - [`logged_op`] refuses to write past a per-client ceiling on operations.
//!
//! Together they turn a hot loop, which used to be an out-of-memory death, into
//! a loud and fast failure.
//!
//! # Two elections, not one
//!
//! A drawn run may also convert two clients into [`elector_role`] clients, which
//! run the recipe's own `LeaderElector` against an election of its own. That
//! half is judged differently and deliberately so: the recipe owns its
//! transactions, so there is no log to wrap, and
//! [`elector_invariants`] instead pairs the recipe's own history subspace with
//! what the role recorded about its beliefs and its fenced writes, in commit
//! order. The question it answers is about effects rather than code paths: did
//! a write ever land outside the term that authorized it.
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
pub mod elector_invariants;
mod elector_role;
pub mod invariants;
mod liveness;
pub mod log_schema;
mod logged_op;
pub mod replay;
mod roles;
mod swarm;
mod timer;
mod workload;

pub use workload::LeaderElectionWorkload;
