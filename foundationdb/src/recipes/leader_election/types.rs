// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Public types for the Dynamo-style lease protocol.

use super::{LeaderElectionError, Result};
use crate::recipes::ranked_register::Rank;
use std::time::Duration;

/// Identifies one process incarnation participating in an election.
///
/// Callers must use a fresh ID after process restart. Concurrent callers using
/// one ID remain data-safe through fencing ranks, but cannot safely coordinate
/// leader work and are therefore protocol misuse.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ParticipantId(String);

impl ParticipantId {
    /// Creates a non-empty process-incarnation ID.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(value)))]
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty() {
            return Err(LeaderElectionError::InvalidParticipantId);
        }
        Ok(Self(value))
    }

    /// Returns the process-incarnation ID as text.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Caller-owned state carried between successful outer transactions.
///
/// No variant is persisted. The time values are meaningful only to the one
/// caller's monotonic clock and must be adopted only after `Database::run`
/// succeeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalState {
    /// No durable state has been observed by this caller.
    Unknown,
    /// A durable owner that this caller is not currently authorized to renew.
    Observation(Observation),
    /// The exact durable owner record this caller may renew before it expires.
    Leadership(Leadership),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PendingNextState {
    PreservedObservation(Observation),
    NewObservation {
        owner: ParticipantId,
        rank: Rank,
        lease_duration: Duration,
    },
    Leadership(Leadership),
}

impl PendingNextState {
    pub(super) fn preserve_observation(observation: Observation) -> Self {
        Self::PreservedObservation(observation)
    }

    pub(super) fn new_observation(
        owner: ParticipantId,
        rank: Rank,
        lease_duration: Duration,
    ) -> Self {
        Self::NewObservation {
            owner,
            rank,
            lease_duration,
        }
    }

    pub(super) fn leadership(leadership: Leadership) -> Self {
        Self::Leadership(leadership)
    }

    fn into_local_state(self, adopted_at: Duration) -> LocalState {
        match self {
            Self::PreservedObservation(observation) => LocalState::Observation(observation),
            Self::NewObservation {
                owner,
                rank,
                lease_duration,
            } => LocalState::Observation(Observation::new(owner, rank, lease_duration, adopted_at)),
            Self::Leadership(leadership) => LocalState::Leadership(leadership),
        }
    }
}

impl LocalState {
    /// Returns the initial state for a caller that has not polled yet.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug"))]
    pub fn unknown() -> Self {
        Self::Unknown
    }

    /// Returns the observed foreign state, if any.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn observation(&self) -> Option<&Observation> {
        match self {
            Self::Observation(observation) => Some(observation),
            Self::Unknown | Self::Leadership(_) => None,
        }
    }

    /// Returns the leadership token, if any.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn leadership(&self) -> Option<&Leadership> {
        match self {
            Self::Leadership(leadership) => Some(leadership),
            Self::Unknown | Self::Observation(_) => None,
        }
    }
}

/// An exact durable owner record adopted after a successful outer transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    owner: ParticipantId,
    rank: Rank,
    lease_duration: Duration,
    first_observed_at: Duration,
}

impl Observation {
    pub(crate) fn new(
        owner: ParticipantId,
        rank: Rank,
        lease_duration: Duration,
        first_observed_at: Duration,
    ) -> Self {
        Self {
            owner,
            rank,
            lease_duration,
            first_observed_at,
        }
    }

    /// Returns the durable owner observed by this caller.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn owner(&self) -> &ParticipantId {
        &self.owner
    }

    /// Returns the exact observed durable revision as a fencing rank.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn rank(&self) -> Rank {
        self.rank
    }

    /// Returns the lease duration persisted with the observed revision.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn lease_duration(&self) -> Duration {
        self.lease_duration
    }

    /// Returns the caller-clock time when this observation was adopted.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn first_observed_at(&self) -> Duration {
        self.first_observed_at
    }
}

/// The exact durable owner record a caller may renew before local expiry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leadership {
    participant: ParticipantId,
    rank: Rank,
    lease_duration: Duration,
    last_renewed_at: Duration,
}

impl Leadership {
    pub(crate) fn new(
        participant: ParticipantId,
        rank: Rank,
        lease_duration: Duration,
        last_renewed_at: Duration,
    ) -> Self {
        Self {
            participant,
            rank,
            lease_duration,
            last_renewed_at,
        }
    }

    /// Returns the process incarnation authorized by this token.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn participant(&self) -> &ParticipantId {
        &self.participant
    }

    /// Returns this token's durable revision as a fencing rank.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn rank(&self) -> Rank {
        self.rank
    }

    /// Returns the lease duration persisted with this token.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn lease_duration(&self) -> Duration {
        self.lease_duration
    }

    /// Returns the caller-clock time supplied at the start of the successful attempt.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn last_renewed_at(&self) -> Duration {
        self.last_renewed_at
    }
}

/// The role prepared by one poll transaction attempt.
///
/// It is valid only after the enclosing `Database::run` succeeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollOutcome {
    /// The caller has a fresh fencing rank for this transaction attempt.
    Leader {
        rank: Rank,
        transition: PollTransition,
    },
    /// A durable owner was observed, but no leadership transition was prepared.
    Follower {
        owner: ParticipantId,
        rank: Rank,
        lease_duration: Duration,
    },
}

/// The state-machine transition prepared by one poll attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollTransition {
    /// A released or never-created state was acquired immediately.
    Acquired,
    /// An exact, locally unexpired leadership token was renewed.
    Renewed,
    /// An unchanged observed owner was replaced after its persisted duration.
    TookOver,
    /// An expired observation of this same participant was acquired again.
    Reacquired,
    /// A durable owner was observed without a permitted leadership transition.
    Followed,
}

impl PollOutcome {
    /// Returns whether this attempt prepared leadership.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn is_leader(&self) -> bool {
        matches!(self, Self::Leader { .. })
    }

    /// Returns the durable revision as a ranked-register fencing token.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn rank(&self) -> Rank {
        match self {
            Self::Leader { rank, .. } | Self::Follower { rank, .. } => *rank,
        }
    }

    /// Returns the state-machine transition prepared by this attempt.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn transition(&self) -> PollTransition {
        match self {
            Self::Leader { transition, .. } => *transition,
            Self::Follower { .. } => PollTransition::Followed,
        }
    }

    /// Returns whether this attempt replaced a non-released durable owner.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn is_takeover(&self) -> bool {
        self.transition() == PollTransition::TookOver
    }

    /// Returns whether this attempt reacquired an expired record of this participant.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn is_reacquisition(&self) -> bool {
        self.transition() == PollTransition::Reacquired
    }

    /// Returns the observed owner when this attempt prepared follower state.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn owner(&self) -> Option<&ParticipantId> {
        match self {
            Self::Follower { owner, .. } => Some(owner),
            Self::Leader { .. } => None,
        }
    }

    /// Returns the observed persisted lease duration when following.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn lease_duration(&self) -> Option<Duration> {
        match self {
            Self::Follower { lease_duration, .. } => Some(*lease_duration),
            Self::Leader { .. } => None,
        }
    }
}

/// The result prepared by [`LeaderElection::poll`](super::LeaderElection::poll).
///
/// It is valid only after the enclosing `Database::run` succeeds. Before then,
/// the transaction can retry, be cancelled, or fail to commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollResult {
    outcome: PollOutcome,
    pending_next_state: PendingNextState,
}

impl PollResult {
    pub(super) fn new(outcome: PollOutcome, pending_next_state: PendingNextState) -> Self {
        Self {
            outcome,
            pending_next_state,
        }
    }

    /// Returns the prepared role and fencing rank.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn outcome(&self) -> &PollOutcome {
        &self.outcome
    }

    /// Consumes this result and returns caller-local state after outer transaction success.
    ///
    /// `adopted_at` must be read from the caller's monotonic clock after the
    /// enclosing `Database::run` succeeds. It timestamps only a new or reset
    /// observation; an unchanged observation and leadership token retain their
    /// original attempt-local timestamps.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn into_next_state(self, adopted_at: Duration) -> LocalState {
        self.pending_next_state.into_local_state(adopted_at)
    }
}

/// A read-only snapshot of durable state, with no liveness claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElectionState {
    owner: Option<ParticipantId>,
    rank: Rank,
    lease_duration: Option<Duration>,
}

impl ElectionState {
    pub(crate) fn new(
        owner: Option<ParticipantId>,
        rank: Rank,
        lease_duration: Option<Duration>,
    ) -> Self {
        Self {
            owner,
            rank,
            lease_duration,
        }
    }

    /// Returns the durable owner, if the state is currently owned.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn owner(&self) -> Option<&ParticipantId> {
        self.owner.as_ref()
    }

    /// Returns the durable revision as a fencing rank.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn rank(&self) -> Rank {
        self.rank
    }

    /// Returns the last persisted lease duration, if this state was created.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn lease_duration(&self) -> Option<Duration> {
        self.lease_duration
    }
}

/// Result of a conditional resignation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResignOutcome {
    /// The matching owner token staged a release in the current transaction.
    Resigned,
    /// The durable owner or revision had already changed.
    Rejected,
}

impl ResignOutcome {
    /// Returns whether the current transaction staged the matching resignation.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn is_resigned(&self) -> bool {
        matches!(self, Self::Resigned)
    }
}
