// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Public values exchanged with the Dynamo-style lease protocol.

use super::{LeaderElectionError, Result};
use crate::recipes::ranked_register::Rank;
use std::time::Duration;

/// Identifies one caller process incarnation participating in an election.
///
/// Keep one ID for the lifetime of a process incarnation, then use a fresh ID
/// after restart. Reusing an ID across concurrent callers is protocol misuse:
/// fencing ranks preserve durable data safety, but the callers cannot safely
/// coordinate leadership or protected work as one incarnation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ParticipantId(String);

impl ParticipantId {
    /// Maximum tuple-encoded size of a process-incarnation ID.
    ///
    /// The limit includes the tuple string type code, terminator, and escaping
    /// of embedded NUL bytes. It leaves enough space for the rest of the
    /// durable election state below FoundationDB's 100,000-byte value limit.
    pub const MAX_ENCODED_BYTES: usize = 95_000;

    /// Creates a non-empty process-incarnation ID within the encoded-size limit.
    ///
    /// The value is persisted as the durable owner when this participant leads,
    /// so it must distinguish a restarted process from its previous incarnation.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(value)))]
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty() {
            return Err(LeaderElectionError::InvalidParticipantId);
        }
        let encoded_size = value
            .bytes()
            .filter(|byte| *byte == 0)
            .fold(value.len().saturating_add(2), |size, _| {
                size.saturating_add(1)
            });
        if encoded_size > Self::MAX_ENCODED_BYTES {
            return Err(LeaderElectionError::ParticipantIdTooLarge {
                encoded_size,
                limit: Self::MAX_ENCODED_BYTES,
            });
        }
        Ok(Self(value))
    }

    /// Returns the caller-supplied process-incarnation ID as text.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn participant_id_accepts_the_encoded_size_limit() {
        let value = "\0".repeat((ParticipantId::MAX_ENCODED_BYTES - 2) / 2);

        assert!(ParticipantId::new(value).is_ok());
    }

    #[test]
    fn participant_id_rejects_an_encoded_size_above_the_limit() {
        let value = "\0".repeat(ParticipantId::MAX_ENCODED_BYTES / 2);

        match ParticipantId::new(value) {
            Err(LeaderElectionError::ParticipantIdTooLarge {
                encoded_size,
                limit,
            }) => {
                assert_eq!(encoded_size, ParticipantId::MAX_ENCODED_BYTES + 2);
                assert_eq!(limit, ParticipantId::MAX_ENCODED_BYTES);
            }
            result => panic!("expected an oversized participant ID error, got {result:?}"),
        }
    }

    const LEASE: Duration = Duration::from_secs(10);

    fn secs(value: u64) -> Duration {
        Duration::from_secs(value)
    }

    fn observed(owner: &str, rank: u64, lease_duration: Duration, at: Duration) -> LocalState {
        LocalState::Observation(Observation::new(
            ParticipantId::new(owner).unwrap(),
            Rank::from(rank),
            lease_duration,
            at,
        ))
    }

    fn next_observation(owner: &str, rank: u64, timer: ObservationTimer) -> NextState {
        NextState::Observation {
            owner: ParticipantId::new(owner).unwrap(),
            rank: Rank::from(rank),
            lease_duration: LEASE,
            timer,
        }
    }

    #[test]
    fn input_measures_elapsed_from_the_anchor_and_saturates() {
        let leader = LocalState::Leadership(Leadership::new(Rank::from(3), LEASE, secs(5)));
        assert_eq!(LocalState::unknown().input(secs(9)), PollInput::Unknown);
        assert_eq!(
            leader.input(secs(9)),
            PollInput::Leadership {
                rank: Rank::from(3),
                lease_duration: LEASE,
                elapsed: secs(4),
            }
        );
        assert_eq!(
            observed("a", 3, LEASE, secs(5)).input(secs(2)),
            PollInput::Observation {
                owner: ParticipantId::new("a").unwrap(),
                rank: Rank::from(3),
                lease_duration: LEASE,
                elapsed: Duration::ZERO,
            }
        );
    }

    #[test]
    fn adopt_anchors_leadership_at_attempt_start() {
        let next = NextState::Leadership {
            rank: Rank::from(4),
            lease_duration: LEASE,
        };
        for local in [LocalState::unknown(), observed("a", 3, LEASE, secs(1))] {
            let adopted = local.adopt(secs(5), secs(9), &next);
            let leadership = adopted.leadership().unwrap();
            assert_eq!(leadership.rank(), Rank::from(4));
            assert_eq!(leadership.lease_duration(), LEASE);
            assert_eq!(leadership.last_renewed_at(), secs(5));
        }
    }

    #[test]
    fn adopt_anchors_a_reset_observation_after_success() {
        let adopted = observed("a", 3, LEASE, secs(1)).adopt(
            secs(5),
            secs(9),
            &next_observation("a", 3, ObservationTimer::Reset),
        );
        assert_eq!(adopted, observed("a", 3, LEASE, secs(9)));
    }

    #[test]
    fn adopt_preserve_keeps_the_matching_anchor() {
        let adopted = observed("a", 3, LEASE, secs(1)).adopt(
            secs(5),
            secs(9),
            &next_observation("a", 3, ObservationTimer::Preserve),
        );
        assert_eq!(adopted, observed("a", 3, LEASE, secs(1)));
    }

    #[test]
    fn adopt_preserve_falls_back_to_reset_when_self_does_not_match() {
        let next = next_observation("a", 3, ObservationTimer::Preserve);
        for local in [
            LocalState::unknown(),
            LocalState::Leadership(Leadership::new(Rank::from(3), LEASE, secs(1))),
            observed("b", 3, LEASE, secs(1)),
            observed("a", 2, LEASE, secs(1)),
            observed("a", 3, LEASE + Duration::from_nanos(1), secs(1)),
        ] {
            assert_eq!(
                local.adopt(secs(5), secs(9), &next),
                observed("a", 3, LEASE, secs(9))
            );
        }
    }
}

/// What the polling caller claims about the durable state, with the time
/// elapsed since its anchor measured on the caller's own monotonic clock.
///
/// The recipe holds no time: it only compares the claimed tuple with durable
/// state and `elapsed` with the claimed `lease_duration`. Local callers build
/// it with [`LocalState::input`]. A transport service relaying a remote caller
/// builds it from the request and must authenticate that caller; the tuple
/// check only protects against stale callers, and the received `elapsed` is
/// trusted as is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollInput {
    /// The caller has not adopted any poll result. It follows any durable owner.
    Unknown,
    /// An exact durable owner record the caller observed and is not authorized to renew.
    ///
    /// Only an exact match of `owner`, `rank`, and `lease_duration` with
    /// durable state, with `elapsed` at least `lease_duration`, permits a
    /// takeover (or a reacquisition when `owner` is the polling participant).
    Observation {
        owner: ParticipantId,
        rank: Rank,
        lease_duration: Duration,
        elapsed: Duration,
    },
    /// The durable owner record the polling participant claims to hold.
    ///
    /// The owner is the participant passed to
    /// [`super::LeaderElection::poll`]. It renews only on an exact durable
    /// match with `elapsed` strictly below `lease_duration`.
    Leadership {
        rank: Rank,
        lease_duration: Duration,
        elapsed: Duration,
    },
}

/// Whether a follower's observation timer restarts or keeps running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationTimer {
    /// The observed record is new or changed: anchor a fresh timer once the
    /// enclosing transaction is known to have committed.
    Reset,
    /// The observed record is exactly the one claimed by the input: keep the
    /// existing anchor.
    Preserve,
}

/// The clock-free state a caller carries to its next poll.
///
/// It holds no time. The caller anchors it on its own monotonic clock, see
/// [`LocalState::adopt`] for the anchoring rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NextState {
    /// The polling participant staged itself as owner of this exact record.
    ///
    /// `lease_duration` is the polling handle's configured duration, which is
    /// the one persisted with `rank`.
    Leadership {
        rank: Rank,
        lease_duration: Duration,
    },
    /// The exact durable owner record observed by this poll.
    Observation {
        owner: ParticipantId,
        rank: Rank,
        lease_duration: Duration,
        timer: ObservationTimer,
    },
}

/// Optional caller-owned helper holding the local anchors between polls.
///
/// No variant is persisted or transferable to another process incarnation.
/// Its anchors are readings of the caller's monotonic clock. Build each
/// poll's [`PollInput`] with [`Self::input`], then replace it with
/// [`Self::adopt`] only after the enclosing
/// [`Database::run`](crate::Database::run) succeeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalState {
    /// No durable state has been adopted by this caller.
    Unknown,
    /// An exact durable owner record this caller is not authorized to renew.
    ///
    /// The preserved observation time can permit a conditional takeover only
    /// if a later poll sees the same owner, revision, and lease duration.
    Observation(Observation),
    /// The exact durable owner record this caller may attempt to renew locally.
    ///
    /// It does not prove current durable ownership. A later poll must still
    /// match the record and find this caller's local lease interval unexpired.
    Leadership(Leadership),
}

impl LocalState {
    /// Returns the initial state for a caller that has not adopted a poll result.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug"))]
    pub fn unknown() -> Self {
        Self::Unknown
    }

    /// Returns the poll input claiming this state, with elapsed time measured to `now`.
    ///
    /// `now` is a reading of the caller's monotonic clock taken before the
    /// enclosing [`Database::run`](crate::Database::run). Keep the same input
    /// for every retry attempt of that run. Elapsed time saturates at zero if
    /// `now` precedes the anchor.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn input(&self, now: Duration) -> PollInput {
        match self {
            Self::Unknown => PollInput::Unknown,
            Self::Observation(observation) => PollInput::Observation {
                owner: observation.owner.clone(),
                rank: observation.rank,
                lease_duration: observation.lease_duration,
                elapsed: now.saturating_sub(observation.first_observed_at),
            },
            Self::Leadership(leadership) => PollInput::Leadership {
                rank: leadership.rank,
                lease_duration: leadership.lease_duration,
                elapsed: now.saturating_sub(leadership.last_renewed_at),
            },
        }
    }

    /// Anchors a committed poll's [`NextState`] and returns the state for the next poll.
    ///
    /// Call it only after the enclosing [`Database::run`](crate::Database::run)
    /// succeeds. `attempt_started_at` is the reading passed to [`Self::input`]
    /// for that run and `adopted_at` a fresh reading taken after it succeeded.
    ///
    /// - Leadership is anchored at `attempt_started_at`, before the durable
    ///   read, so read, retry, and commit delay only shorten local validity.
    /// - A reset observation is anchored at `adopted_at`, so its timer starts
    ///   only once its durable read is known to have committed.
    /// - A preserved observation keeps this state's anchor when this state is
    ///   the same observation, and otherwise falls back to a reset.
    #[cfg_attr(
        feature = "trace",
        tracing::instrument(level = "debug", skip(self, next))
    )]
    pub fn adopt(
        self,
        attempt_started_at: Duration,
        adopted_at: Duration,
        next: &NextState,
    ) -> Self {
        match next {
            NextState::Leadership {
                rank,
                lease_duration,
            } => Self::Leadership(Leadership::new(*rank, *lease_duration, attempt_started_at)),
            NextState::Observation {
                owner,
                rank,
                lease_duration,
                timer,
            } => match self {
                Self::Observation(observation)
                    if *timer == ObservationTimer::Preserve
                        && observation.owner == *owner
                        && observation.rank == *rank
                        && observation.lease_duration == *lease_duration =>
                {
                    Self::Observation(observation)
                }
                _ => Self::Observation(Observation::new(
                    owner.clone(),
                    *rank,
                    *lease_duration,
                    adopted_at,
                )),
            },
        }
    }

    /// Returns the adopted observation, if this caller is following an owner.
    ///
    /// `None` means either no state has been adopted or this caller holds a
    /// local leadership token. It does not query durable state.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn observation(&self) -> Option<&Observation> {
        match self {
            Self::Observation(observation) => Some(observation),
            Self::Unknown | Self::Leadership(_) => None,
        }
    }

    /// Returns the adopted local leadership token, if any.
    ///
    /// The returned token is input to a later [`super::LeaderElection::poll`]
    /// or [`super::LeaderElection::resign`] call, not proof that the caller
    /// remains the durable owner.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn leadership(&self) -> Option<&Leadership> {
        match self {
            Self::Leadership(leadership) => Some(leadership),
            Self::Unknown | Self::Observation(_) => None,
        }
    }
}

/// An exact durable owner record adopted after a successful outer transaction.
///
/// This records the owner, revision, and persisted duration observed together,
/// plus the caller-clock instant at which that result was adopted. It is valid
/// for takeover timing only while a later poll finds the same durable record.
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

    /// Returns the owner in the exact durable record this caller observed.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn owner(&self) -> &ParticipantId {
        &self.owner
    }

    /// Returns the exact observed durable revision as a fencing rank.
    ///
    /// A changed rank makes this observation ineligible to authorize a
    /// takeover, even if the owner text is unchanged.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn rank(&self) -> Rank {
        self.rank
    }

    /// Returns the lease duration persisted with the observed revision.
    ///
    /// Followers use this value, rather than a handle's configured duration,
    /// when deciding whether the unchanged record is old enough to challenge.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn lease_duration(&self) -> Duration {
        self.lease_duration
    }

    /// Returns the caller-clock time when this observation was adopted.
    ///
    /// Compare it only with readings from the same caller's monotonic clock.
    /// It is not a durable timestamp or a deadline for the observed owner.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn first_observed_at(&self) -> Duration {
        self.first_observed_at
    }
}

/// The exact durable owner record a caller may attempt to renew before local expiry.
///
/// It is caller-local evidence held by [`LocalState`] for the participant that
/// polled, not a lease granted by a durable clock: a poll must still verify
/// the participant, rank, and persisted duration against durable state. A
/// later successful leader poll, including renewal by this same participant,
/// supersedes this token's fencing rank.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leadership {
    rank: Rank,
    lease_duration: Duration,
    last_renewed_at: Duration,
}

impl Leadership {
    pub(crate) fn new(rank: Rank, lease_duration: Duration, last_renewed_at: Duration) -> Self {
        Self {
            rank,
            lease_duration,
            last_renewed_at,
        }
    }

    /// Returns this token's durable revision as a fencing rank.
    ///
    /// A later successful leader poll supersedes this rank, including renewal
    /// by the same participant.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn rank(&self) -> Rank {
        self.rank
    }

    /// Returns the lease duration persisted with this token.
    ///
    /// It bounds local renewability from [`Self::last_renewed_at`], not the
    /// durable owner's lifetime.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn lease_duration(&self) -> Duration {
        self.lease_duration
    }

    /// Returns the caller-clock reading taken before the successful poll's run.
    ///
    /// A renewal is locally eligible only while elapsed time from this value is
    /// less than [`Self::lease_duration`]. It must be compared only with the
    /// same caller's monotonic clock.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn last_renewed_at(&self) -> Duration {
        self.last_renewed_at
    }
}

/// The role and fencing rank prepared by one poll transaction attempt.
///
/// The rank may protect work staged in the same transaction, but it authorizes
/// no committed or external work until the enclosing
/// [`Database::run`](crate::Database::run) succeeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollOutcome {
    /// The transaction staged this caller as owner with a fresh fencing rank.
    ///
    /// [`PollTransition`] classifies how that staged ownership was reached.
    Leader {
        rank: Rank,
        transition: PollTransition,
    },
    /// A durable owner was observed, but no leadership transition was staged.
    ///
    /// The fields form the exact record carried by
    /// [`NextState::Observation`].
    Follower {
        owner: ParticipantId,
        rank: Rank,
        lease_duration: Duration,
    },
}

/// The state-machine transition classified by one poll attempt.
///
/// This is an outcome label, not separately persisted durable state. It is
/// meaningful only with the [`PollOutcome`] produced by the committed outer
/// transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollTransition {
    /// A released or never-created durable state was acquired immediately.
    Acquired,
    /// An exact, locally unexpired [`Leadership`] token was renewed.
    Renewed,
    /// An unchanged foreign [`Observation`] was replaced after its persisted duration.
    TookOver,
    /// An expired [`Observation`] of this same participant was acquired again.
    Reacquired,
    /// A durable owner was observed without a permitted leadership transition.
    ///
    /// This is returned for [`PollOutcome::Follower`] and never stages an
    /// ownership mutation.
    Followed,
}

impl PollOutcome {
    /// Returns whether this attempt staged an ownership transition.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn is_leader(&self) -> bool {
        matches!(self, Self::Leader { .. })
    }

    /// Returns the observed or newly staged durable revision as a fencing rank.
    ///
    /// Only [`Self::is_leader`] outcomes provide a new rank that can protect
    /// work staged by this poll transaction.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn rank(&self) -> Rank {
        match self {
            Self::Leader { rank, .. } | Self::Follower { rank, .. } => *rank,
        }
    }

    /// Returns the transition classification for this attempt.
    ///
    /// [`PollOutcome::Follower`] always reports [`PollTransition::Followed`].
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn transition(&self) -> PollTransition {
        match self {
            Self::Leader { transition, .. } => *transition,
            Self::Follower { .. } => PollTransition::Followed,
        }
    }

    /// Returns whether this attempt staged replacement of a foreign owner.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn is_takeover(&self) -> bool {
        self.transition() == PollTransition::TookOver
    }

    /// Returns whether this attempt staged reacquisition of this participant's expired record.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn is_reacquisition(&self) -> bool {
        self.transition() == PollTransition::Reacquired
    }

    /// Returns the observed owner when this attempt produced follower state.
    ///
    /// The owner is an observation, not an authorization for the caller.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn owner(&self) -> Option<&ParticipantId> {
        match self {
            Self::Follower { owner, .. } => Some(owner),
            Self::Leader { .. } => None,
        }
    }

    /// Returns the observed persisted lease duration when following.
    ///
    /// This duration is relevant to a later poll only with the matching
    /// observation carried by [`PollResult::next`].
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn lease_duration(&self) -> Option<Duration> {
        match self {
            Self::Follower { lease_duration, .. } => Some(*lease_duration),
            Self::Leader { .. } => None,
        }
    }
}

/// The result prepared by [`super::LeaderElection::poll`].
///
/// Keep this value inside the transaction callback until the enclosing
/// [`Database::run`](crate::Database::run) succeeds. Before then, the
/// transaction can retry, be cancelled, or fail to commit. On success, inspect
/// [`Self::outcome`] and carry [`Self::next`] into the next poll, for example
/// with [`LocalState::adopt`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollResult {
    outcome: PollOutcome,
    next: NextState,
}

impl PollResult {
    pub(super) fn new(outcome: PollOutcome, next: NextState) -> Self {
        Self { outcome, next }
    }

    /// Returns the role and fencing rank prepared by this transaction attempt.
    ///
    /// See [`PollOutcome`] for when the rank is usable for protected work.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn outcome(&self) -> &PollOutcome {
        &self.outcome
    }

    /// Returns the clock-free state to carry into the next poll.
    ///
    /// Adopt it only after the enclosing [`Database::run`](crate::Database::run)
    /// succeeds, anchoring it on the caller's clock as described by
    /// [`LocalState::adopt`].
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn next(&self) -> &NextState {
        &self.next
    }
}

/// A read-only snapshot of durable state for diagnostics and observability.
///
/// It makes no liveness, expiry, or leadership-validity claim. Use
/// [`super::LeaderElection::poll`] with a caller-owned [`PollInput`] for
/// protocol decisions instead of deriving authority from this snapshot.
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

    /// Returns the durable owner, if the observed state is not released.
    ///
    /// `Some` does not show whether that owner is running or locally renewable.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn owner(&self) -> Option<&ParticipantId> {
        self.owner.as_ref()
    }

    /// Returns the durable revision as a fencing rank.
    ///
    /// A rank is retained after resignation so a later acquisition receives a
    /// strictly newer fencing epoch.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn rank(&self) -> Rank {
        self.rank
    }

    /// Returns the last persisted lease duration, if this state has been created.
    ///
    /// It is historical state, not a persisted expiration deadline.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn lease_duration(&self) -> Option<Duration> {
        self.lease_duration
    }
}

/// Result of a conditional resignation attempt.
///
/// It describes what the current transaction staged and is final only after
/// the enclosing [`Database::run`](crate::Database::run) succeeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResignOutcome {
    /// The exact claimed owner record staged a release in the current transaction.
    Resigned,
    /// The durable owner, revision, or persisted duration no longer matched.
    ///
    /// No release mutation was staged, preventing an old delayed resignation
    /// from releasing a newer leader.
    Rejected,
}

impl ResignOutcome {
    /// Returns whether the current transaction staged the matching resignation.
    ///
    /// This is not proof of release until the outer transaction commits.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn is_resigned(&self) -> bool {
        matches!(self, Self::Resigned)
    }
}
