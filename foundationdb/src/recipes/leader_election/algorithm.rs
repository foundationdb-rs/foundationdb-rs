// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Transactional implementation of the poll-based election state machine.

use crate::{
    Transaction,
    recipes::ranked_register::Rank,
    tuple::{Subspace, pack, unpack},
};
use std::ops::Deref;
use std::time::Duration;

use super::{
    ElectionState, LeaderElectionError, Observation, ParticipantId, PollOutcome, PollResult,
    ResignOutcome, Result, keys::state_key,
};

#[derive(Debug, Clone, Default)]
struct DurableState {
    generation: u64,
    owner: Option<ParticipantId>,
}

async fn read_state<T>(txn: &T, key: &[u8]) -> Result<DurableState>
where
    T: Deref<Target = Transaction>,
{
    let Some(value) = txn.get(key, false).await? else {
        return Ok(DurableState::default());
    };

    let (generation, has_owner, owner): (u64, bool, String) = unpack(&value)?;
    let owner = if has_owner {
        Some(ParticipantId::new(owner)?)
    } else if owner.is_empty() {
        None
    } else {
        return Err(LeaderElectionError::InvalidState(
            "unowned state contains an owner ID".to_owned(),
        ));
    };
    if owner.is_some() && generation == 0 {
        return Err(LeaderElectionError::InvalidState(
            "owned state has zero generation".to_owned(),
        ));
    }

    Ok(DurableState { generation, owner })
}

fn write_state<T>(txn: &T, key: &[u8], state: &DurableState)
where
    T: Deref<Target = Transaction>,
{
    let owner = state.owner.as_ref().map_or("", ParticipantId::as_str);
    txn.set(
        key,
        &pack(&(state.generation, state.owner.is_some(), owner)),
    );
}

fn next_generation(state: &DurableState) -> Result<u64> {
    state
        .generation
        .checked_add(1)
        .ok_or(LeaderElectionError::GenerationExhausted)
}

fn observation(state: &DurableState, observed_at: Duration) -> Observation {
    Observation {
        generation: Some(state.generation),
        owner: state.owner.clone(),
        observed_at,
    }
}

fn is_same_observation(state: &DurableState, previous: &Observation) -> bool {
    previous.generation == Some(state.generation) && previous.owner == state.owner
}

pub(crate) async fn poll<T>(
    txn: &T,
    subspace: &Subspace,
    suspicion_duration: Duration,
    participant: &ParticipantId,
    previous: &Observation,
    now: Duration,
) -> Result<PollResult>
where
    T: Deref<Target = Transaction>,
{
    let key = state_key(subspace);
    let state = read_state(txn, &key).await?;

    let incumbent = state.owner.as_ref() == Some(participant);
    let unowned = state.owner.is_none();
    let same_observation = is_same_observation(&state, previous);
    let suspected = same_observation
        && state.owner.as_ref() != Some(participant)
        && now.saturating_sub(previous.observed_at) >= suspicion_duration;

    if incumbent || unowned || suspected {
        let generation = next_generation(&state)?;
        let takeover = suspected;
        let next_state = DurableState {
            generation,
            owner: Some(participant.clone()),
        };
        write_state(txn, &key, &next_state);

        #[cfg(feature = "trace")]
        tracing::debug!(
            poll_outcome = "leader",
            generation,
            owner_transition = !incumbent,
            takeover,
            "leader-election poll staged in transaction"
        );

        return Ok(PollResult::new(
            PollOutcome::Leader {
                rank: Rank::from(generation),
                takeover,
            },
            observation(&next_state, now),
        ));
    }

    let owner = state.owner.clone().ok_or_else(|| {
        LeaderElectionError::InvalidState("follower poll observed no owner".to_owned())
    })?;
    #[cfg(feature = "trace")]
    tracing::debug!(
        poll_outcome = "follower",
        generation = state.generation,
        owner_transition = false,
        takeover = false,
        "leader-election poll observed owner"
    );

    let next_observation = if same_observation {
        previous.clone()
    } else {
        observation(&state, now)
    };

    Ok(PollResult::new(
        PollOutcome::Follower {
            owner,
            rank: Rank::from(state.generation),
        },
        next_observation,
    ))
}

pub(crate) async fn state<T>(txn: &T, subspace: &Subspace) -> Result<ElectionState>
where
    T: Deref<Target = Transaction>,
{
    let state = read_state(txn, &state_key(subspace)).await?;
    Ok(ElectionState::new(
        state.owner,
        Rank::from(state.generation),
    ))
}

pub(crate) async fn resign<T>(
    txn: &T,
    subspace: &Subspace,
    participant: &ParticipantId,
    rank: Rank,
) -> Result<ResignOutcome>
where
    T: Deref<Target = Transaction>,
{
    let key = state_key(subspace);
    let state = read_state(txn, &key).await?;
    if state.generation != rank.as_u64() || state.owner.as_ref() != Some(participant) {
        #[cfg(feature = "trace")]
        tracing::debug!(
            resign_outcome = "rejected",
            generation = state.generation,
            stale_rank = rank.as_u64(),
            "leader-election stale resignation rejected"
        );
        return Ok(ResignOutcome::Rejected);
    }

    let resigned = DurableState {
        generation: state.generation,
        owner: None,
    };
    write_state(txn, &key, &resigned);
    #[cfg(feature = "trace")]
    tracing::debug!(
        resign_outcome = "resigned",
        generation = resigned.generation,
        "leader-election resignation staged in transaction"
    );
    Ok(ResignOutcome::Resigned)
}
