// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Transactional implementation of the Dynamo-style lease state machine.

use crate::{
    Transaction,
    recipes::ranked_register::Rank,
    tuple::{Subspace, pack, unpack},
};
use std::ops::Deref;
use std::time::Duration;

use super::{
    ElectionState, LeaderElectionError, NextState, ObservationTimer, ParticipantId, PollInput,
    PollOutcome, PollResult, PollTransition, ResignOutcome, Result, keys::state_key,
};

const STATE_SCHEMA_VERSION: u64 = 1;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct DurableState {
    revision: u64,
    owner: Option<ParticipantId>,
    lease_duration: Option<Duration>,
}

async fn read_state<T>(txn: &T, key: &[u8]) -> Result<DurableState>
where
    T: Deref<Target = Transaction>,
{
    let Some(value) = txn.get(key, false).await? else {
        return Ok(DurableState::default());
    };

    decode_state(&value)
}

fn decode_state(value: &[u8]) -> Result<DurableState> {
    let (
        schema_version,
        revision,
        has_owner,
        owner,
        has_lease_duration,
        lease_duration_secs,
        lease_duration_subsec_nanos,
    ): (u64, u64, bool, String, bool, u64, u32) = unpack(value)?;
    if schema_version != STATE_SCHEMA_VERSION {
        return Err(LeaderElectionError::InvalidState(format!(
            "unknown durable state schema version {schema_version}, expected {STATE_SCHEMA_VERSION}"
        )));
    }
    let owner = if has_owner {
        Some(ParticipantId::new(owner)?)
    } else if owner.is_empty() {
        None
    } else {
        return Err(LeaderElectionError::InvalidState(
            "released state contains an owner ID".to_owned(),
        ));
    };
    if !has_lease_duration && (lease_duration_secs != 0 || lease_duration_subsec_nanos != 0) {
        return Err(LeaderElectionError::InvalidState(
            "missing lease duration has non-zero fields".to_owned(),
        ));
    }
    if has_lease_duration && lease_duration_subsec_nanos >= 1_000_000_000 {
        return Err(LeaderElectionError::InvalidState(
            "lease duration subsecond nanos is out of range".to_owned(),
        ));
    }
    let lease_duration =
        has_lease_duration.then(|| Duration::new(lease_duration_secs, lease_duration_subsec_nanos));
    if revision == 0 && (owner.is_some() || lease_duration.is_some()) {
        return Err(LeaderElectionError::InvalidState(
            "zero revision state is not empty".to_owned(),
        ));
    }
    if revision > 0 && lease_duration.is_none() {
        return Err(LeaderElectionError::InvalidState(
            "created state has no lease duration".to_owned(),
        ));
    }
    if lease_duration == Some(Duration::ZERO) {
        return Err(LeaderElectionError::InvalidState(
            "persisted lease duration is zero".to_owned(),
        ));
    }
    Ok(DurableState {
        revision,
        owner,
        lease_duration,
    })
}

fn write_state<T>(txn: &T, key: &[u8], state: &DurableState)
where
    T: Deref<Target = Transaction>,
{
    txn.set(key, &encode_state(state));
}

fn encode_state(state: &DurableState) -> Vec<u8> {
    let owner = state.owner.as_ref().map_or("", ParticipantId::as_str);
    let (lease_duration_secs, lease_duration_subsec_nanos) =
        state.lease_duration.map_or((0, 0), |duration| {
            (duration.as_secs(), duration.subsec_nanos())
        });
    pack(&(
        STATE_SCHEMA_VERSION,
        state.revision,
        state.owner.is_some(),
        owner,
        state.lease_duration.is_some(),
        lease_duration_secs,
        lease_duration_subsec_nanos,
    ))
}

fn next_revision(state: &DurableState) -> Result<u64> {
    state
        .revision
        .checked_add(1)
        .ok_or(LeaderElectionError::RevisionExhausted)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    Leader(PollTransition),
    Follow {
        timer: ObservationTimer,
        reason: &'static str,
    },
}

fn same_record(
    state: &DurableState,
    owner: &ParticipantId,
    rank: Rank,
    lease_duration: Duration,
) -> bool {
    state.owner.as_ref() == Some(owner)
        && state.revision == rank.as_u64()
        && state.lease_duration == Some(lease_duration)
}

fn decide(state: &DurableState, participant: &ParticipantId, input: &PollInput) -> Decision {
    if state.owner.is_none() {
        return Decision::Leader(PollTransition::Acquired);
    }
    match input {
        PollInput::Leadership {
            rank,
            lease_duration,
            elapsed,
        } if same_record(state, participant, *rank, *lease_duration)
            && elapsed < lease_duration =>
        {
            Decision::Leader(PollTransition::Renewed)
        }
        PollInput::Leadership { .. } => Decision::Follow {
            timer: ObservationTimer::Reset,
            reason: "leadership_not_renewable",
        },
        PollInput::Observation {
            owner,
            rank,
            lease_duration,
            elapsed,
        } if same_record(state, owner, *rank, *lease_duration) => {
            if elapsed >= lease_duration {
                Decision::Leader(if owner == participant {
                    PollTransition::Reacquired
                } else {
                    PollTransition::TookOver
                })
            } else {
                Decision::Follow {
                    timer: ObservationTimer::Preserve,
                    reason: "observation",
                }
            }
        }
        PollInput::Observation { .. } => Decision::Follow {
            timer: ObservationTimer::Reset,
            reason: "observation",
        },
        PollInput::Unknown => Decision::Follow {
            timer: ObservationTimer::Reset,
            reason: "unknown",
        },
    }
}

fn leader_result(
    state: &DurableState,
    participant: &ParticipantId,
    lease_duration: Duration,
    transition: PollTransition,
) -> Result<(PollResult, DurableState)> {
    let revision = next_revision(state)?;
    let next_state = DurableState {
        revision,
        owner: Some(participant.clone()),
        lease_duration: Some(lease_duration),
    };
    let rank = Rank::from(revision);

    #[cfg(feature = "trace")]
    let action = match transition {
        PollTransition::Acquired => "acquisition",
        PollTransition::Renewed => "renewal",
        PollTransition::TookOver => "takeover",
        PollTransition::Reacquired => "reacquisition",
        PollTransition::Followed => "follower",
    };

    #[cfg(feature = "trace")]
    tracing::debug!(
        poll_outcome = "leader",
        poll_action = action,
        participant = participant.as_str(),
        revision,
        takeover = transition == PollTransition::TookOver,
        reacquisition = transition == PollTransition::Reacquired,
        "leader-election poll staged in transaction"
    );

    Ok((
        PollResult::new(
            PollOutcome::Leader { rank, transition },
            NextState::Leadership {
                rank,
                lease_duration,
            },
        ),
        next_state,
    ))
}

fn follower_result(
    state: &DurableState,
    timer: ObservationTimer,
    _reason: &'static str,
) -> Result<PollResult> {
    let owner = state.owner.clone().ok_or_else(|| {
        LeaderElectionError::InvalidState(
            "follower result requested from released state".to_owned(),
        )
    })?;
    let lease_duration = state.lease_duration.ok_or_else(|| {
        LeaderElectionError::InvalidState(
            "follower result requested without lease duration".to_owned(),
        )
    })?;
    let rank = Rank::from(state.revision);

    #[cfg(feature = "trace")]
    tracing::debug!(
        poll_outcome = "follower",
        poll_reason = _reason,
        owner = owner.as_str(),
        revision = rank.as_u64(),
        preserve_timer = timer == ObservationTimer::Preserve,
        "leader-election poll observed owner"
    );

    let next = NextState::Observation {
        owner: owner.clone(),
        rank,
        lease_duration,
        timer,
    };
    let outcome = PollOutcome::Follower {
        owner,
        rank,
        lease_duration,
    };

    Ok(PollResult::new(outcome, next))
}

/// Pure poll step: the result and, for a leader outcome, the state to write.
fn step(
    state: &DurableState,
    participant: &ParticipantId,
    lease_duration: Duration,
    input: &PollInput,
) -> Result<(PollResult, Option<DurableState>)> {
    match decide(state, participant, input) {
        Decision::Leader(transition) => {
            let (result, next_state) =
                leader_result(state, participant, lease_duration, transition)?;
            Ok((result, Some(next_state)))
        }
        Decision::Follow { timer, reason } => Ok((follower_result(state, timer, reason)?, None)),
    }
}

pub(crate) async fn poll<T>(
    txn: &T,
    subspace: &Subspace,
    lease_duration: Duration,
    participant: &ParticipantId,
    input: &PollInput,
) -> Result<PollResult>
where
    T: Deref<Target = Transaction>,
{
    let key = state_key(subspace);
    let state = read_state(txn, &key).await?;
    let (result, next_state) = step(&state, participant, lease_duration, input)?;
    if let Some(next_state) = next_state {
        write_state(txn, &key, &next_state);
    }
    Ok(result)
}

pub(crate) async fn state<T>(txn: &T, subspace: &Subspace) -> Result<ElectionState>
where
    T: Deref<Target = Transaction>,
{
    let state = read_state(txn, &state_key(subspace)).await?;
    Ok(ElectionState::new(
        state.owner,
        Rank::from(state.revision),
        state.lease_duration,
    ))
}

pub(crate) async fn resign<T>(
    txn: &T,
    subspace: &Subspace,
    participant: &ParticipantId,
    rank: Rank,
    lease_duration: Duration,
) -> Result<ResignOutcome>
where
    T: Deref<Target = Transaction>,
{
    let key = state_key(subspace);
    let state = read_state(txn, &key).await?;
    if !same_record(&state, participant, rank, lease_duration) {
        #[cfg(feature = "trace")]
        let rejection_reason = if state.owner.as_ref() != Some(participant) {
            "owner_changed"
        } else if state.revision != rank.as_u64() {
            "revision_changed"
        } else {
            "lease_duration_changed"
        };
        #[cfg(feature = "trace")]
        tracing::debug!(
            resign_outcome = "rejected",
            resign_reason = rejection_reason,
            participant = participant.as_str(),
            leadership_revision = rank.as_u64(),
            revision = state.revision,
            "leader-election stale resignation rejected"
        );
        return Ok(ResignOutcome::Rejected);
    }

    let released = DurableState {
        revision: state.revision,
        owner: None,
        lease_duration: state.lease_duration,
    };
    write_state(txn, &key, &released);
    #[cfg(feature = "trace")]
    tracing::debug!(
        resign_outcome = "resigned",
        revision = released.revision,
        "leader-election resignation staged in transaction"
    );
    Ok(ResignOutcome::Resigned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipes::leader_election::{Leadership, LocalState, Observation};

    #[test]
    fn durable_state_version_one_round_trips() {
        let state = DurableState {
            revision: 42,
            owner: Some(ParticipantId::new("alice-incarnation").unwrap()),
            lease_duration: Some(Duration::new(5, 123)),
        };

        assert_eq!(decode_state(&encode_state(&state)).unwrap(), state);
    }

    #[test]
    fn maximum_participant_id_keeps_durable_state_below_fdb_value_limit() {
        let owner =
            ParticipantId::new("\0".repeat((ParticipantId::MAX_ENCODED_BYTES - 2) / 2)).unwrap();
        assert_eq!(
            pack(&owner.as_str()).len(),
            ParticipantId::MAX_ENCODED_BYTES
        );
        let state = DurableState {
            revision: u64::MAX,
            owner: Some(owner),
            lease_duration: Some(Duration::new(u64::MAX, 999_999_999)),
        };

        assert!(encode_state(&state).len() <= 100_000);
    }

    #[test]
    fn unknown_durable_state_schema_version_is_rejected() {
        let value = pack(&(2_u64, 1_u64, true, "alice", true, 5_u64, 0_u32));

        assert!(matches!(
            decode_state(&value),
            Err(LeaderElectionError::InvalidState(message))
                if message.contains("unknown durable state schema version 2")
        ));
    }

    #[test]
    fn untagged_six_field_durable_state_is_rejected() {
        let value = pack(&(1_u64, true, "alice", true, 5_u64, 0_u32));

        assert!(matches!(
            decode_state(&value),
            Err(LeaderElectionError::PackError(_))
        ));
    }

    // Literal bytes: a change here breaks every durable state already written.
    #[test]
    fn durable_state_version_one_encoding_is_stable() {
        let cases: [(DurableState, &[u8]); 3] = [
            (
                DurableState {
                    revision: 3,
                    owner: None,
                    lease_duration: Some(Duration::from_secs(10)),
                },
                &[21, 1, 21, 3, 38, 2, 0, 39, 21, 10, 20],
            ),
            (
                DurableState {
                    revision: 7,
                    owner: Some(ParticipantId::new("a").unwrap()),
                    lease_duration: Some(Duration::new(10, 500)),
                },
                &[21, 1, 21, 7, 39, 2, 97, 0, 39, 21, 10, 22, 1, 244],
            ),
            (DurableState::default(), &[21, 1, 20, 38, 2, 0, 38, 20, 20]),
        ];

        for (state, bytes) in cases {
            assert_eq!(encode_state(&state), bytes);
            assert_eq!(decode_state(bytes).unwrap(), state);
        }
    }

    const LEASE: Duration = Duration::from_secs(10);
    const NANO: Duration = Duration::from_nanos(1);

    fn id(value: &str) -> ParticipantId {
        ParticipantId::new(value).unwrap()
    }

    fn owned(owner: &str) -> DurableState {
        DurableState {
            revision: 7,
            owner: Some(id(owner)),
            lease_duration: Some(LEASE),
        }
    }

    fn observation(owner: &ParticipantId, elapsed: Duration) -> PollInput {
        PollInput::Observation {
            owner: owner.clone(),
            rank: Rank::from(7),
            lease_duration: LEASE,
            elapsed,
        }
    }

    fn leadership(elapsed: Duration) -> PollInput {
        PollInput::Leadership {
            rank: Rank::from(7),
            lease_duration: LEASE,
            elapsed,
        }
    }

    const RESET: Decision = Decision::Follow {
        timer: ObservationTimer::Reset,
        reason: "observation",
    };
    const NOT_RENEWABLE: Decision = Decision::Follow {
        timer: ObservationTimer::Reset,
        reason: "leadership_not_renewable",
    };

    #[test]
    fn vacant_state_is_acquired_whatever_the_input() {
        let me = id("me");
        let other = id("other");
        for state in [
            DurableState::default(),
            DurableState {
                revision: 7,
                owner: None,
                lease_duration: Some(LEASE),
            },
        ] {
            for input in [
                PollInput::Unknown,
                observation(&other, Duration::ZERO),
                observation(&me, LEASE),
                leadership(Duration::ZERO),
                leadership(LEASE),
            ] {
                assert_eq!(
                    decide(&state, &me, &input),
                    Decision::Leader(PollTransition::Acquired)
                );
            }
        }
    }

    #[test]
    fn leadership_renews_strictly_before_lease_end() {
        let me = id("me");
        let state = owned("me");

        assert_eq!(
            decide(&state, &me, &leadership(LEASE - NANO)),
            Decision::Leader(PollTransition::Renewed)
        );
        assert_eq!(decide(&state, &me, &leadership(LEASE)), NOT_RENEWABLE);
    }

    #[test]
    fn changed_record_is_not_renewable() {
        let me = id("me");
        let changed = [
            owned("other"),
            DurableState {
                revision: 8,
                ..owned("me")
            },
            DurableState {
                lease_duration: Some(LEASE + NANO),
                ..owned("me")
            },
        ];

        for state in changed {
            assert_eq!(
                decide(&state, &me, &leadership(Duration::ZERO)),
                NOT_RENEWABLE
            );
        }
    }

    #[test]
    fn unchanged_observation_is_taken_over_at_lease_end() {
        let me = id("me");
        let other = id("other");
        let state = owned("other");

        assert_eq!(
            decide(&state, &me, &observation(&other, LEASE - NANO)),
            Decision::Follow {
                timer: ObservationTimer::Preserve,
                reason: "observation",
            }
        );
        assert_eq!(
            decide(&state, &me, &observation(&other, LEASE)),
            Decision::Leader(PollTransition::TookOver)
        );
    }

    #[test]
    fn own_expired_observation_is_reacquired() {
        let me = id("me");

        assert_eq!(
            decide(&owned("me"), &me, &observation(&me, LEASE)),
            Decision::Leader(PollTransition::Reacquired)
        );
    }

    #[test]
    fn changed_observation_resets_the_timer() {
        let me = id("me");
        let other = id("other");
        let changed = [
            owned("third"),
            DurableState {
                revision: 8,
                ..owned("other")
            },
            DurableState {
                lease_duration: Some(LEASE + NANO),
                ..owned("other")
            },
        ];

        for state in changed {
            assert_eq!(decide(&state, &me, &observation(&other, LEASE)), RESET);
        }
    }

    #[test]
    fn unknown_follows_the_owner() {
        assert_eq!(
            decide(&owned("other"), &id("me"), &PollInput::Unknown),
            Decision::Follow {
                timer: ObservationTimer::Reset,
                reason: "unknown",
            }
        );
    }

    /// The pre-clock-free poll logic, copied verbatim from the parent of the
    /// `decide` extraction and made pure: it computes the would-be written
    /// state instead of staging it, and adopts the pending state directly.
    mod legacy {
        use super::*;

        #[derive(Debug, Clone, PartialEq, Eq)]
        pub(super) struct Observation {
            pub(super) owner: ParticipantId,
            pub(super) rank: Rank,
            pub(super) lease_duration: Duration,
            pub(super) first_observed_at: Duration,
        }

        #[derive(Debug, Clone, PartialEq, Eq)]
        pub(super) struct Leadership {
            pub(super) participant: ParticipantId,
            pub(super) rank: Rank,
            pub(super) lease_duration: Duration,
            pub(super) last_renewed_at: Duration,
        }

        #[derive(Debug, Clone, PartialEq, Eq)]
        pub(super) enum LocalState {
            Unknown,
            Observation(Observation),
            Leadership(Leadership),
        }

        enum PendingNextState {
            PreservedObservation(Observation),
            NewObservation {
                owner: ParticipantId,
                rank: Rank,
                lease_duration: Duration,
            },
            Leadership(Leadership),
        }

        impl PendingNextState {
            fn into_local_state(self, adopted_at: Duration) -> LocalState {
                match self {
                    Self::PreservedObservation(observation) => LocalState::Observation(observation),
                    Self::NewObservation {
                        owner,
                        rank,
                        lease_duration,
                    } => LocalState::Observation(Observation {
                        owner,
                        rank,
                        lease_duration,
                        first_observed_at: adopted_at,
                    }),
                    Self::Leadership(leadership) => LocalState::Leadership(leadership),
                }
            }
        }

        fn same_observation(state: &DurableState, observation: &Observation) -> bool {
            state.owner.as_ref() == Some(&observation.owner)
                && state.revision == observation.rank.as_u64()
                && state.lease_duration == Some(observation.lease_duration)
        }

        fn valid_leadership(
            state: &DurableState,
            participant: &ParticipantId,
            leadership: &Leadership,
            now: Duration,
        ) -> bool {
            &leadership.participant == participant
                && state.owner.as_ref() == Some(participant)
                && state.revision == leadership.rank.as_u64()
                && state.lease_duration == Some(leadership.lease_duration)
                && now.saturating_sub(leadership.last_renewed_at) < leadership.lease_duration
        }

        fn leader_result(
            state: &DurableState,
            participant: &ParticipantId,
            lease_duration: Duration,
            attempt_started_at: Duration,
            transition: PollTransition,
        ) -> (PollOutcome, PendingNextState) {
            let rank = Rank::from(next_revision(state).unwrap());
            (
                PollOutcome::Leader { rank, transition },
                PendingNextState::Leadership(Leadership {
                    participant: participant.clone(),
                    rank,
                    lease_duration,
                    last_renewed_at: attempt_started_at,
                }),
            )
        }

        fn follower_result(
            state: &DurableState,
            previous: Option<&Observation>,
        ) -> (PollOutcome, PendingNextState) {
            let owner = state.owner.clone().unwrap();
            let lease_duration = state.lease_duration.unwrap();
            let rank = Rank::from(state.revision);
            let next_observation = match previous {
                Some(previous) if same_observation(state, previous) => {
                    PendingNextState::PreservedObservation(previous.clone())
                }
                Some(_) | None => PendingNextState::NewObservation {
                    owner: owner.clone(),
                    rank,
                    lease_duration,
                },
            };
            (
                PollOutcome::Follower {
                    owner,
                    rank,
                    lease_duration,
                },
                next_observation,
            )
        }

        pub(super) fn step(
            state: &DurableState,
            lease_duration: Duration,
            participant: &ParticipantId,
            local_state: &LocalState,
            attempt_started_at: Duration,
            adopted_at: Duration,
        ) -> (PollOutcome, LocalState) {
            let (outcome, pending) = match local_state {
                LocalState::Leadership(leadership)
                    if valid_leadership(state, participant, leadership, attempt_started_at) =>
                {
                    leader_result(
                        state,
                        participant,
                        lease_duration,
                        attempt_started_at,
                        PollTransition::Renewed,
                    )
                }
                LocalState::Observation(observation) if state.owner.is_some() => {
                    if same_observation(state, observation)
                        && attempt_started_at.saturating_sub(observation.first_observed_at)
                            >= observation.lease_duration
                    {
                        let transition = if &observation.owner == participant {
                            PollTransition::Reacquired
                        } else {
                            PollTransition::TookOver
                        };
                        leader_result(
                            state,
                            participant,
                            lease_duration,
                            attempt_started_at,
                            transition,
                        )
                    } else {
                        follower_result(state, Some(observation))
                    }
                }
                LocalState::Unknown if state.owner.is_some() => follower_result(state, None),
                LocalState::Leadership(_) if state.owner.is_some() => follower_result(state, None),
                LocalState::Observation(_) | LocalState::Unknown | LocalState::Leadership(_) => {
                    leader_result(
                        state,
                        participant,
                        lease_duration,
                        attempt_started_at,
                        PollTransition::Acquired,
                    )
                }
            };
            (outcome, pending.into_local_state(adopted_at))
        }
    }

    /// Maps a legacy local state to the new helper. A leadership owned by
    /// another participant is not expressible and claimed nothing: `Unknown`.
    fn to_new(local: &legacy::LocalState, participant: &ParticipantId) -> LocalState {
        match local {
            legacy::LocalState::Unknown => LocalState::Unknown,
            legacy::LocalState::Observation(observation) => {
                LocalState::Observation(Observation::new(
                    observation.owner.clone(),
                    observation.rank,
                    observation.lease_duration,
                    observation.first_observed_at,
                ))
            }
            legacy::LocalState::Leadership(leadership)
                if &leadership.participant != participant =>
            {
                LocalState::Unknown
            }
            legacy::LocalState::Leadership(leadership) => LocalState::Leadership(Leadership::new(
                leadership.rank,
                leadership.lease_duration,
                leadership.last_renewed_at,
            )),
        }
    }

    /// Compares a resulting local state; a resulting legacy leadership is
    /// always owned by the poller, so only its rank, duration, and anchor count.
    fn same_local(new: &LocalState, legacy: &legacy::LocalState) -> bool {
        match (new, legacy) {
            (LocalState::Unknown, legacy::LocalState::Unknown) => true,
            (LocalState::Observation(new), legacy::LocalState::Observation(legacy)) => {
                new.owner() == &legacy.owner
                    && new.rank() == legacy.rank
                    && new.lease_duration() == legacy.lease_duration
                    && new.first_observed_at() == legacy.first_observed_at
            }
            (LocalState::Leadership(new), legacy::LocalState::Leadership(legacy)) => {
                new.rank() == legacy.rank
                    && new.lease_duration() == legacy.lease_duration
                    && new.last_renewed_at() == legacy.last_renewed_at
            }
            _ => false,
        }
    }

    #[test]
    fn clock_free_poll_matches_legacy_poll_exhaustively() {
        let secs = Duration::from_secs;
        let participants = [id("a"), id("b")];
        let durations = [secs(1), secs(2)];
        let anchors: Vec<Duration> = (0..=5).map(secs).collect();

        let mut states = Vec::new();
        for revision in 0..=3_u64 {
            for owner in [None, Some(id("a")), Some(id("b"))] {
                for lease_duration in durations {
                    let state = match (revision, &owner) {
                        (0, None) => DurableState::default(),
                        (0, Some(_)) => continue,
                        _ => DurableState {
                            revision,
                            owner: owner.clone(),
                            lease_duration: Some(lease_duration),
                        },
                    };
                    if !states.contains(&state) {
                        states.push(state);
                    }
                }
            }
        }

        let mut cases = 0_usize;
        for participant in &participants {
            let mut locals = vec![legacy::LocalState::Unknown];
            for rank in (0..=3).map(Rank::from) {
                for lease_duration in durations {
                    for &anchor in &anchors {
                        for owner in &participants {
                            locals.push(legacy::LocalState::Observation(legacy::Observation {
                                owner: owner.clone(),
                                rank,
                                lease_duration,
                                first_observed_at: anchor,
                            }));
                            // Legacy leadership tokens: the poller's own and a foreign one.
                            locals.push(legacy::LocalState::Leadership(legacy::Leadership {
                                participant: owner.clone(),
                                rank,
                                lease_duration,
                                last_renewed_at: anchor,
                            }));
                        }
                    }
                }
            }

            for state in &states {
                for local in &locals {
                    let new_local = to_new(local, participant);
                    for handle_lease in durations {
                        for now in (0..=6).map(secs) {
                            let adopted_at = now + secs(1);
                            let (legacy_outcome, legacy_next) = legacy::step(
                                state,
                                handle_lease,
                                participant,
                                local,
                                now,
                                adopted_at,
                            );
                            let (result, written) =
                                step(state, participant, handle_lease, &new_local.input(now))
                                    .unwrap();
                            let new_next = new_local.clone().adopt(now, adopted_at, result.next());

                            let context = format!(
                                "state {state:?}, participant {participant:?}, local {local:?}, \
                                 handle lease {handle_lease:?}, now {now:?}"
                            );
                            assert_eq!(result.outcome(), &legacy_outcome, "{context}");
                            assert!(same_local(&new_next, &legacy_next), "{context}");
                            let expected_written =
                                legacy_outcome.is_leader().then(|| DurableState {
                                    revision: legacy_outcome.rank().as_u64(),
                                    owner: Some(participant.clone()),
                                    lease_duration: Some(handle_lease),
                                });
                            assert_eq!(written, expected_written, "{context}");
                            cases += 1;
                        }
                    }
                }
            }
        }
        assert!(cases > 100_000, "only {cases} cases");
    }
}
