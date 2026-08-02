// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Public types for the poll-based leader-election protocol.

use super::{LeaderElectionError, Result};
use crate::recipes::ranked_register::Rank;
use std::time::Duration;

/// Identifies one process incarnation participating in an election.
///
/// Callers must use a fresh ID after process restart. Concurrent use of one ID
/// remains data-safe because each successful poll receives a new rank, but it
/// is protocol misuse because the independent callers cannot coordinate work.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ParticipantId(String);

impl ParticipantId {
    /// Creates a non-empty participant ID.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(value)))]
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty() {
            return Err(LeaderElectionError::InvalidParticipantId);
        }
        Ok(Self(value))
    }

    /// Returns the participant ID as text.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A caller-owned local observation of durable leader state.
///
/// The contained time is from one caller's monotonic clock. It is never
/// persisted or compared with another participant's observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub(crate) generation: Option<u64>,
    pub(crate) owner: Option<ParticipantId>,
    pub(crate) observed_at: Duration,
}

impl Observation {
    /// Creates the initial local observation for a participant.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug"))]
    pub fn initial(observed_at: Duration) -> Self {
        Self {
            generation: None,
            owner: None,
            observed_at,
        }
    }

    /// Returns when this observation was made on the caller's monotonic clock.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn observed_at(&self) -> Duration {
        self.observed_at
    }
}

/// The role prepared by one poll transaction attempt.
///
/// It is valid only after the enclosing `Database::run` succeeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollOutcome {
    /// The caller owns the durable state for this poll.
    ///
    /// `rank` must fence correctness-sensitive FoundationDB work through a
    /// [`RankedRegister`](crate::recipes::ranked_register::RankedRegister) in
    /// the same enclosing transaction. External sinks must enforce it too.
    Leader { rank: Rank, takeover: bool },
    /// Another participant owns the durable state.
    Follower { owner: ParticipantId, rank: Rank },
}

impl PollOutcome {
    /// Returns whether this poll made the caller leader.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn is_leader(&self) -> bool {
        matches!(self, Self::Leader { .. })
    }

    /// Returns the durable generation as the ranked-register fencing token.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn rank(&self) -> Rank {
        match self {
            Self::Leader { rank, .. } | Self::Follower { rank, .. } => *rank,
        }
    }

    /// Returns whether leadership changed away from another participant.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn is_takeover(&self) -> bool {
        matches!(self, Self::Leader { takeover: true, .. })
    }
}

/// The result prepared by [`LeaderElection::poll`](super::LeaderElection::poll).
///
/// It is valid only after the enclosing `Database::run` succeeds. Before then,
/// the transaction can retry, be cancelled, or fail to commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollResult {
    outcome: PollOutcome,
    next_observation: Observation,
}

impl PollResult {
    pub(crate) fn new(outcome: PollOutcome, next_observation: Observation) -> Self {
        Self {
            outcome,
            next_observation,
        }
    }

    /// Returns the poll role and its current fencing token.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn outcome(&self) -> &PollOutcome {
        &self.outcome
    }

    /// Returns the observation to adopt after the enclosing transaction commits.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn next_observation(&self) -> &Observation {
        &self.next_observation
    }

    /// Consumes this result and returns the next local observation.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn into_next_observation(self) -> Observation {
        self.next_observation
    }
}

/// A read-only snapshot of durable election state.
///
/// This makes no liveness or leadership-validity claim. `Rank::ZERO` with no
/// owner represents a state key that has never been created. After resignation,
/// the owner is absent but the rank remains non-zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElectionState {
    owner: Option<ParticipantId>,
    rank: Rank,
}

impl ElectionState {
    pub(crate) fn new(owner: Option<ParticipantId>, rank: Rank) -> Self {
        Self { owner, rank }
    }

    /// Returns the participant owning the current durable state, if any.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn owner(&self) -> Option<&ParticipantId> {
        self.owner.as_ref()
    }

    /// Returns the current durable generation as a fencing token.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn rank(&self) -> Rank {
        self.rank
    }
}

/// Result of a conditional resignation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResignOutcome {
    /// The matching owner and generation were cleared.
    Resigned,
    /// The owner or generation had already changed.
    Rejected,
}

impl ResignOutcome {
    /// Returns whether the current transaction staged the matching resignation.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn is_resigned(&self) -> bool {
        matches!(self, Self::Resigned)
    }
}
