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

use super::types::PendingNextState;
use super::{
    ElectionState, LeaderElectionError, Leadership, LocalState, Observation, ParticipantId,
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

/// What the polling participant locally claims about the durable state,
/// with the time elapsed since its anchor already computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Claim<'a> {
    Unknown,
    Observation {
        owner: &'a ParticipantId,
        rank: Rank,
        lease_duration: Duration,
        elapsed: Duration,
    },
    /// Owned by the polling participant.
    Leadership {
        rank: Rank,
        lease_duration: Duration,
        elapsed: Duration,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    Leader(PollTransition),
    /// `preserve` keeps the previous observation and its timer.
    Follow {
        preserve: bool,
        reason: &'static str,
    },
}

fn claim<'a>(
    participant: &ParticipantId,
    local_state: &'a LocalState,
    attempt_started_at: Duration,
) -> Claim<'a> {
    match local_state {
        LocalState::Unknown => Claim::Unknown,
        LocalState::Observation(observation) => Claim::Observation {
            owner: observation.owner(),
            rank: observation.rank(),
            lease_duration: observation.lease_duration(),
            elapsed: attempt_started_at.saturating_sub(observation.first_observed_at()),
        },
        // A leadership token of another participant proves nothing to this one.
        LocalState::Leadership(leadership) if leadership.participant() != participant => {
            Claim::Unknown
        }
        LocalState::Leadership(leadership) => Claim::Leadership {
            rank: leadership.rank(),
            lease_duration: leadership.lease_duration(),
            elapsed: attempt_started_at.saturating_sub(leadership.last_renewed_at()),
        },
    }
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

fn decide(state: &DurableState, participant: &ParticipantId, claim: &Claim) -> Decision {
    if state.owner.is_none() {
        return Decision::Leader(PollTransition::Acquired);
    }
    match *claim {
        Claim::Leadership {
            rank,
            lease_duration,
            elapsed,
        } if same_record(state, participant, rank, lease_duration) && elapsed < lease_duration => {
            Decision::Leader(PollTransition::Renewed)
        }
        Claim::Leadership { .. } => Decision::Follow {
            preserve: false,
            reason: "leadership_not_renewable",
        },
        Claim::Observation {
            owner,
            rank,
            lease_duration,
            elapsed,
        } if same_record(state, owner, rank, lease_duration) => {
            if elapsed >= lease_duration {
                Decision::Leader(if owner == participant {
                    PollTransition::Reacquired
                } else {
                    PollTransition::TookOver
                })
            } else {
                Decision::Follow {
                    preserve: true,
                    reason: "observation",
                }
            }
        }
        Claim::Observation { .. } => Decision::Follow {
            preserve: false,
            reason: "observation",
        },
        Claim::Unknown => Decision::Follow {
            preserve: false,
            reason: "unknown",
        },
    }
}

fn leader_result<T>(
    txn: &T,
    key: &[u8],
    state: &DurableState,
    participant: &ParticipantId,
    lease_duration: Duration,
    attempt_started_at: Duration,
    transition: PollTransition,
) -> Result<PollResult>
where
    T: Deref<Target = Transaction>,
{
    let revision = next_revision(state)?;
    let next_state = DurableState {
        revision,
        owner: Some(participant.clone()),
        lease_duration: Some(lease_duration),
    };
    write_state(txn, key, &next_state);
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

    Ok(PollResult::new(
        PollOutcome::Leader { rank, transition },
        PendingNextState::leadership(Leadership::new(
            participant.clone(),
            rank,
            lease_duration,
            attempt_started_at,
        )),
    ))
}

fn follower_result(
    state: &DurableState,
    preserved: Option<&Observation>,
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
    let next_observation = match preserved {
        Some(preserved) => PendingNextState::preserve_observation(preserved.clone()),
        None => PendingNextState::new_observation(owner.clone(), rank, lease_duration),
    };

    #[cfg(feature = "trace")]
    tracing::debug!(
        poll_outcome = "follower",
        poll_reason = _reason,
        owner = owner.as_str(),
        revision = rank.as_u64(),
        "leader-election poll observed owner"
    );

    let outcome = PollOutcome::Follower {
        owner,
        rank,
        lease_duration,
    };

    Ok(PollResult::new(outcome, next_observation))
}

pub(crate) async fn poll<T>(
    txn: &T,
    subspace: &Subspace,
    lease_duration: Duration,
    participant: &ParticipantId,
    local_state: &LocalState,
    attempt_started_at: Duration,
) -> Result<PollResult>
where
    T: Deref<Target = Transaction>,
{
    let key = state_key(subspace);
    let state = read_state(txn, &key).await?;

    match decide(
        &state,
        participant,
        &claim(participant, local_state, attempt_started_at),
    ) {
        Decision::Leader(transition) => leader_result(
            txn,
            &key,
            &state,
            participant,
            lease_duration,
            attempt_started_at,
            transition,
        ),
        Decision::Follow { preserve, reason } => {
            let preserved = match local_state {
                LocalState::Observation(observation) if preserve => Some(observation),
                _ => None,
            };
            follower_result(&state, preserved, reason)
        }
    }
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
    leadership: &Leadership,
) -> Result<ResignOutcome>
where
    T: Deref<Target = Transaction>,
{
    let key = state_key(subspace);
    let state = read_state(txn, &key).await?;
    if state.owner.as_ref() != Some(leadership.participant())
        || state.revision != leadership.rank().as_u64()
        || state.lease_duration != Some(leadership.lease_duration())
    {
        #[cfg(feature = "trace")]
        let rejection_reason = if state.owner.as_ref() != Some(leadership.participant()) {
            "owner_changed"
        } else if state.revision != leadership.rank().as_u64() {
            "revision_changed"
        } else {
            "lease_duration_changed"
        };
        #[cfg(feature = "trace")]
        tracing::debug!(
            resign_outcome = "rejected",
            resign_reason = rejection_reason,
            participant = leadership.participant().as_str(),
            leadership_revision = leadership.rank().as_u64(),
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

    fn observation(owner: &ParticipantId, elapsed: Duration) -> Claim<'_> {
        Claim::Observation {
            owner,
            rank: Rank::from(7),
            lease_duration: LEASE,
            elapsed,
        }
    }

    fn leadership(elapsed: Duration) -> Claim<'static> {
        Claim::Leadership {
            rank: Rank::from(7),
            lease_duration: LEASE,
            elapsed,
        }
    }

    const RESET: Decision = Decision::Follow {
        preserve: false,
        reason: "observation",
    };
    const NOT_RENEWABLE: Decision = Decision::Follow {
        preserve: false,
        reason: "leadership_not_renewable",
    };

    #[test]
    fn vacant_state_is_acquired_whatever_the_claim() {
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
            for claim in [
                Claim::Unknown,
                observation(&other, Duration::ZERO),
                observation(&me, LEASE),
                leadership(Duration::ZERO),
                leadership(LEASE),
            ] {
                assert_eq!(
                    decide(&state, &me, &claim),
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
                preserve: true,
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
            decide(&owned("other"), &id("me"), &Claim::Unknown),
            Decision::Follow {
                preserve: false,
                reason: "unknown",
            }
        );
    }

    #[test]
    fn foreign_leadership_claims_nothing() {
        let local_state = LocalState::Leadership(Leadership::new(
            id("other"),
            Rank::from(7),
            LEASE,
            Duration::ZERO,
        ));

        assert_eq!(claim(&id("me"), &local_state, LEASE), Claim::Unknown);
        assert_eq!(claim(&id("other"), &local_state, LEASE), leadership(LEASE));
    }
}
