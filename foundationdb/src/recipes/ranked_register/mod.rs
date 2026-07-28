// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! # Ranked Register for FoundationDB
//!
//! A shared memory abstraction that encapsulates Paxos ballots, based on
//! Chockler & Malkhi's "Active Disk Paxos with infinitely many processes"
//! (PODC 2002). A ranked register is a mutable register with conflict detection
//! via ranks, supporting unbounded processes with finite storage.
//!
//! ## Operations
//!
//! | Operation | Who | Effect |
//! |-----------|-----|--------|
//! | [`read(rank)`](crate::recipes::ranked_register::RankedRegister::read) | Leader | Updates max_read_rank (installs fence), returns current value |
//! | [`write(rank, value)`](crate::recipes::ranked_register::RankedRegister::write) | Leader | Commits only if rank is high enough |
//! | [`value()`](crate::recipes::ranked_register::RankedRegister::value) | Followers | Plain read, no fence installed |
//!
//! ## Composing with Leader Election
//!
//! The ranked register is the safety half of an "election service": the
//! [leader election recipe](crate::recipes::leader_election) decides who *may*
//! act, and the register is what stops anybody else from acting. A term's
//! ballot becomes the rank of every operation that term performs.
//!
//! The order below is the contract, not a suggestion. Winning a term fences
//! nothing by itself: the register only starts rejecting the old leader once
//! the new one has installed its fence with [`read`](RankedRegister::read) at
//! its own rank.
//!
//! The example below activates the fence as the first thing the work does,
//! which is the best the handle layer allows: its campaign runs in a
//! transaction the caller does not compose into. The stronger form is available
//! at the primitive layer, where the caller owns the transaction and can put
//! `try_claim` and the fencing `read` in the same one, so the term change and
//! the fence commit atomically. Prefer it when you are driving the primitives:
//! a process that wins a term and then stops before activating leaves the
//! register carrying its old fence, which keeps a *previous* leader able to
//! write until somebody else wins a term and activates.
//!
//! ```rust,no_run
//! use foundationdb::Database;
//! use foundationdb::recipes::leader_election::{LeadOutcome, LeaderElector};
//! use foundationdb::recipes::ranked_register::{RankedRegister, RankedRegisterError};
//! use foundationdb::tuple::Subspace;
//!
//! async fn lead(db: &Database, elector: &LeaderElector) -> Result<(), RankedRegisterError> {
//!     let register = RankedRegister::new(Subspace::all().subspace(&"my-state"));
//!
//!     let outcome = elector
//!         .lead(|handle| async move {
//!             let register = &register;
//!
//!             // Step 1: install the fence, before doing anything the term
//!             // authorizes. `rank(0)` is the first rank of this term.
//!             let fence = handle.rank(0).expect("the term was just won");
//!             let current = db
//!                 .run(|txn, _| async move { register.read(&txn, fence).await })
//!                 .await?;
//!
//!             // Step 2: fenced work. Every write carries a rank of this term,
//!             // so a predecessor that wakes up mid-write is rejected.
//!             let mut next = current.value().unwrap_or(b"").to_vec();
//!             next.push(b'!');
//!             let next = &next;
//!             let rank = handle.next_rank().expect("still leading");
//!             db.run(|txn, _| async move { register.write(&txn, rank, next).await })
//!                 .await?;
//!
//!             Ok::<_, RankedRegisterError>(())
//!         })
//!         .await
//!         .expect("the campaign failed");
//!
//!     match outcome {
//!         LeadOutcome::Completed { value, .. } => value,
//!         // The term ended mid-work and the work future was dropped. Whatever
//!         // it had already written stays; the fence is what makes that safe.
//!         LeadOutcome::LeaseLost => Ok(()),
//!     }
//! }
//!
//! // Followers read with `value()`, which installs no fence and so cannot
//! // disturb the leader.
//! async fn follow(db: &Database, register: &RankedRegister) -> Result<(), RankedRegisterError> {
//!     let _current = db
//!         .run(|txn, _| async move { register.value(&txn).await })
//!         .await?;
//!     Ok(())
//! }
//! ```
//!
//! ### Why This Works
//!
//! - Ballots are monotonic and never reset, even across a resign, so a
//!   dispossessed leader's ballot is always below its successor's.
//! - A rank puts the ballot in the high bits and a per-term sequence in the
//!   low bits, so every rank of term `b + 1` dominates every rank of term `b`.
//! - `read(rank)` installs a fence at that rank; any lower-ranked write is
//!   rejected from then on.
//! - `value()` is safe for followers, since it never installs a fence.
//!
//! The guarantee is transactional and therefore covers FoundationDB-resident
//! state only. Effects outside the database get the ballot as a token, and the
//! systems receiving it have to do their own rejecting.

mod algorithm;
mod errors;
mod keys;
mod types;

pub use errors::{RankedRegisterError, Result};
pub use types::{Rank, ReadResult, RegisterState, WriteResult};

use crate::{Transaction, tuple::Subspace};
use std::ops::Deref;

/// A ranked register backed by FoundationDB
///
/// Provides a mutable register with conflict detection via ranks.
/// No initialization is needed — an absent key represents the bottom state
/// (zero ranks, no value).
///
/// # Thread Safety
///
/// `RankedRegister` is [`Clone`], [`Send`], and [`Sync`]. It holds only a
/// [`Subspace`] and can be safely shared across tasks.
#[derive(Clone, Debug)]
pub struct RankedRegister {
    subspace: Subspace,
}

impl RankedRegister {
    /// Create a new ranked register instance
    ///
    /// The subspace isolates this register from other data in the database.
    /// No initialization step is required — the register starts in the
    /// bottom state (zero ranks, no value) until the first write.
    pub fn new(subspace: Subspace) -> Self {
        Self { subspace }
    }

    /// Returns a reference to the underlying subspace
    pub fn subspace(&self) -> &Subspace {
        &self.subspace
    }

    /// Perform a ranked read
    ///
    /// Updates `max_read_rank` if the given rank is higher, installing a fence
    /// that prevents lower-ranked writes. Returns the current write rank and value.
    ///
    /// Used by the leader before writing to ensure consistency.
    pub async fn read<T>(&self, txn: &T, rank: Rank) -> Result<ReadResult>
    where
        T: Deref<Target = Transaction>,
    {
        algorithm::read(txn, &self.subspace, rank).await
    }

    /// Perform a ranked write
    ///
    /// Commits the value only if:
    /// - `rank >= max_read_rank` (no higher fence)
    /// - `rank > max_write_rank` (no equal-or-higher write)
    ///
    /// Returns [`WriteResult::Committed`] or [`WriteResult::Aborted`].
    pub async fn write<T>(&self, txn: &T, rank: Rank, value: &[u8]) -> Result<WriteResult>
    where
        T: Deref<Target = Transaction>,
    {
        algorithm::write(txn, &self.subspace, rank, value).await
    }

    /// Read the current value without updating ranks
    ///
    /// Safe for followers and observers — does not install a fence,
    /// so it won't interfere with the leader's writes.
    pub async fn value<T>(&self, txn: &T) -> Result<Option<Vec<u8>>>
    where
        T: Deref<Target = Transaction>,
    {
        algorithm::value(txn, &self.subspace).await
    }
}
