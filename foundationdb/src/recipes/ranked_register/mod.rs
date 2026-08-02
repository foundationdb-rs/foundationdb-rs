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
//! The ranked register is designed to work with the leader election recipe.
//! A successful leader poll returns the generation used as the rank for register
//! operations, providing automatic fencing against stale leaders.
//!
//! ```rust,no_run
//! # #[cfg(feature = "recipes-leader-election")]
//! # mod leader_election_example {
//! # async fn example(db: &foundationdb::Database) -> Result<(), foundationdb::FdbBindingError> {
//! use std::time::Duration;
//!
//! use foundationdb::{
//!     options::TransactionOption,
//!     recipes::{
//!         leader_election::{LeaderElection, Observation, ParticipantId, PollOutcome},
//!         ranked_register::RankedRegister,
//!     },
//!     tuple::Subspace,
//!     FdbBindingError,
//! };
//!
//! let election = LeaderElection::new(
//!     Subspace::all().subspace(&"my-election"),
//!     Duration::from_secs(10),
//! );
//! let register = RankedRegister::new(Subspace::all().subspace(&"my-state"));
//! let participant = ParticipantId::new("process-incarnation")?;
//! let observation = Observation::initial(Duration::ZERO);
//!
//! // The application owns retries, options, scheduling, and local observation.
//! let result = db.run(|txn, _maybe_committed| {
//!     let election = election.clone();
//!     let register = register.clone();
//!     let participant = participant.clone();
//!     let observation = observation.clone();
//!     async move {
//!         txn.set_option(TransactionOption::AutomaticIdempotency)?;
//!         let poll = election.poll(&txn, &participant, &observation, Duration::ZERO).await?;
//!         if let PollOutcome::Leader { rank, .. } = poll.outcome() {
//!             register
//!                 .read(&txn, *rank)
//!                 .await
//!                 .map_err(|error| FdbBindingError::new_custom_error(Box::new(error)))?;
//!             register
//!                 .write(&txn, *rank, b"new_value")
//!                 .await
//!                 .map_err(|error| FdbBindingError::new_custom_error(Box::new(error)))?;
//!         }
//!         Ok::<_, FdbBindingError>(poll)
//!     }
//! }).await?;
//! // Adopt this only after db.run succeeded.
//! let observation = result.into_next_observation();
//! # let _ = observation;
//! # Ok(())
//! # }
//! # }
//! ```
//!
//! ### Why This Works
//!
//! - Leader-election generations increase monotonically
//! - A deposed leader has a lower generation than the new leader
//! - `read(rank)` installs a fence at the generation value
//! - Any write with a lower rank is automatically rejected
//! - `value()` is safe for followers, it never installs a fence

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
