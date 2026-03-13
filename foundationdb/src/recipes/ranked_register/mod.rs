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
//! | [`read(rank)`](RankedRegister::read) | Leader | Updates max_read_rank (installs fence), returns current value |
//! | [`write(rank, value)`](RankedRegister::write) | Leader | Commits only if rank is high enough |
//! | [`value()`](RankedRegister::value) | Followers | Plain read, no fence installed |
//!
//! ## Composing with Leader Election
//!
//! The ranked register is designed to work with the leader election recipe.
//! The leader election's ballot serves as the rank for register operations,
//! providing automatic fencing against stale leaders.
//!
//! ```rust,no_run
//! # fn example() {
//! use foundationdb::recipes::leader_election::LeaderElection;
//! use foundationdb::recipes::ranked_register::{RankedRegister, Rank};
//! use foundationdb::tuple::Subspace;
//!
//! let election = LeaderElection::new(Subspace::all().subspace(&"my-election"));
//! let register = RankedRegister::new(Subspace::all().subspace(&"my-state"));
//!
//! // In the leader's main loop:
//! // db.run(|txn, _| async move {
//! //     let result = election.run_election_cycle(&txn, process_id, priority, now).await?;
//! //     match result {
//! //         ElectionResult::Leader(state) => {
//! //             let rank = Rank::from(state.ballot);
//! //             // Read current state (installs fence at this ballot)
//! //             let current = register.read(&txn, rank).await?;
//! //             // Mutate and write back
//! //             register.write(&txn, rank, b"new_value").await?;
//! //         }
//! //         ElectionResult::Follower(_) => {
//! //             // Safe read — doesn't interfere with leader's writes
//! //             let current = register.value(&txn).await?;
//! //         }
//! //     }
//! //     Ok(())
//! // }).await?;
//! # }
//! ```
//!
//! ### Why This Works
//!
//! - Leader election's ballot is monotonically increasing
//! - A deposed leader has a lower ballot than the new leader
//! - `read(rank)` installs a fence at the ballot value
//! - Any write with a lower rank is automatically rejected
//! - `value()` is safe for followers — it never installs a fence

mod algorithm;
mod errors;
mod keys;
mod types;

pub use errors::{RankedRegisterError, Result};
pub use types::{Rank, ReadResult, RegisterState, WriteResult};

use crate::{tuple::Subspace, Transaction};
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
