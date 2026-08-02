// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Poll-based, fenced leader election.
//!
//! `LeaderElection` does not run a task, sleep, own a retry loop, or persist a
//! clock reading. The caller invokes [`LeaderElection::poll`] in its own
//! transaction and owns the schedule. A follower only suspects an unchanged
//! owner after elapsed time on its own monotonic clock. Suspicion is advisory:
//! the transaction's serializable read and write on one state key decides the
//! winner.
//!
//! A successful leader poll always returns a fresh, monotonically increasing
//! [`Rank`](crate::recipes::ranked_register::Rank). Leadership alone never
//! authorizes an unfenced external side effect. Correctness-sensitive FDB work
//! must use this rank with a
//! [`RankedRegister`](crate::recipes::ranked_register::RankedRegister) in the
//! same enclosing transaction; external sinks must enforce the token too.
//!
//! `poll` is pure with respect to caller-local state. The caller must adopt the
//! returned observation only after its enclosing `Database::run` succeeds.
//! That makes retry, cancellation, and unknown-commit handling safe: a later
//! poll rediscovers or supersedes any committed state before doing fenced work.

mod algorithm;
mod errors;
mod keys;
mod types;

pub use errors::{LeaderElectionError, Result};
pub use types::{
    ElectionState, Observation, ParticipantId, PollOutcome, PollResult, ResignOutcome,
};

use crate::{Transaction, tuple::Subspace};
use std::ops::Deref;
use std::time::Duration;

/// A handle for one independently scoped leader election.
///
/// The handle contains only durable-key scope and local suspicion policy. It
/// never calls `Database::run`, never retries, and never sets transaction
/// options. Participant IDs identify process incarnations and must be unique
/// across restarts.
#[derive(Clone, Debug)]
pub struct LeaderElection {
    subspace: Subspace,
    suspicion_duration: Duration,
}

impl LeaderElection {
    /// Creates an election handle with the given local takeover-suspicion duration.
    #[cfg_attr(
        feature = "trace",
        tracing::instrument(level = "debug", skip(subspace))
    )]
    pub fn new(subspace: Subspace, suspicion_duration: Duration) -> Self {
        Self {
            subspace,
            suspicion_duration,
        }
    }

    /// Returns the local elapsed-time threshold used only for takeover suspicion.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn suspicion_duration(&self) -> Duration {
        self.suspicion_duration
    }

    /// Polls the election state in the caller's transaction.
    ///
    /// An unowned state is acquired immediately. An incumbent receives a fresh
    /// rank on every successful poll. A follower first records the exact
    /// generation and owner it observed. Only when that exact state remains
    /// unchanged for at least `suspicion_duration` on this caller's monotonic
    /// clock may it attempt takeover. The single-key transactional read/write
    /// is the compare-and-swap that resolves concurrent polls.
    ///
    /// The caller must persist `PollResult::next_observation` only after the
    /// enclosing `Database::run` returns successfully. If this returns leader,
    /// use the returned rank to fence protected work in this same transaction.
    #[cfg_attr(
        feature = "trace",
        tracing::instrument(level = "debug", skip(self, txn, participant, previous))
    )]
    pub async fn poll<T>(
        &self,
        txn: &T,
        participant: &ParticipantId,
        previous: &Observation,
        now: Duration,
    ) -> Result<PollResult>
    where
        T: Deref<Target = Transaction>,
    {
        algorithm::poll(
            txn,
            &self.subspace,
            self.suspicion_duration,
            participant,
            previous,
            now,
        )
        .await
    }

    /// Reads durable state without making any liveness claim.
    ///
    /// The returned generation is always observable, including after
    /// resignation. It is `Rank::ZERO` only when the state key has never been
    /// created.
    #[cfg_attr(
        feature = "trace",
        tracing::instrument(level = "debug", skip(self, txn))
    )]
    pub async fn state<T>(&self, txn: &T) -> Result<ElectionState>
    where
        T: Deref<Target = Transaction>,
    {
        algorithm::state(txn, &self.subspace).await
    }

    /// Clears ownership only when both participant and rank still match.
    ///
    /// A stale delayed resignation is rejected, leaving newer leadership intact.
    /// The durable generation is preserved, so reacquisition always produces a
    /// strictly newer fencing token.
    #[cfg_attr(
        feature = "trace",
        tracing::instrument(level = "debug", skip(self, txn, participant))
    )]
    pub async fn resign<T>(
        &self,
        txn: &T,
        participant: &ParticipantId,
        rank: crate::recipes::ranked_register::Rank,
    ) -> Result<ResignOutcome>
    where
        T: Deref<Target = Transaction>,
    {
        algorithm::resign(txn, &self.subspace, participant, rank).await
    }
}
