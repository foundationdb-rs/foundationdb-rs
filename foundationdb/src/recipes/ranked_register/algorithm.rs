// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Ranked register algorithm
//!
//! Implements the core read/write/value operations from Chockler & Malkhi's
//! "Active Disk Paxos with infinitely many processes" (PODC 2002, Section 5.1, Figure 3).
//!
//! All functions operate within a FoundationDB transaction for atomicity.

use crate::{
    Transaction,
    tuple::{Subspace, pack, unpack},
};
use std::ops::Deref;

use super::{
    MAX_ENCODED_REGISTER_STATE_BYTES,
    errors::{RankedRegisterError, Result},
    keys,
    types::*,
};

// ============================================================================
// STATE HELPERS
// ============================================================================

/// Read the register state from FoundationDB
///
/// Returns `Default` (zero ranks, no value) if the key is absent,
/// which represents the bottom/uninitialized state.
async fn read_state<T>(txn: &T, key: &[u8]) -> Result<RegisterState>
where
    T: Deref<Target = Transaction>,
{
    let data = match txn.get(key, false).await? {
        Some(d) => d,
        None => return Ok(RegisterState::default()),
    };

    // Unpack tuple: (max_read_rank, max_write_rank, has_value, value)
    let tuple: (u64, u64, bool, Vec<u8>) = unpack(&data)?;

    let value = if tuple.2 { Some(tuple.3) } else { None };

    Ok(RegisterState {
        max_read_rank: Rank::from(tuple.0),
        max_write_rank: Rank::from(tuple.1),
        value,
    })
}

/// Write the register state to FoundationDB
fn write_state<T>(txn: &T, key: &[u8], state: &RegisterState) -> Result<()>
where
    T: Deref<Target = Transaction>,
{
    let packed = encode_state(state)?;
    txn.set(key, &packed);
    Ok(())
}

fn encode_state(state: &RegisterState) -> Result<Vec<u8>> {
    let has_value = state.value.is_some();
    let value = state.value.as_deref().unwrap_or(&[]);

    let data = (
        state.max_read_rank.as_u64(),
        state.max_write_rank.as_u64(),
        has_value,
        value,
    );
    let packed = pack(&data);
    if packed.len() > MAX_ENCODED_REGISTER_STATE_BYTES {
        return Err(RankedRegisterError::EncodedStateTooLarge {
            encoded_size: packed.len(),
            limit: MAX_ENCODED_REGISTER_STATE_BYTES,
        });
    }
    Ok(packed)
}

/// Ensures a value remains encodable after a later ranked read installs the
/// largest possible fence. Without this headroom, a near-cap write could make
/// a future fence installation fail only because its rank encoding is larger.
fn ensure_value_survives_future_fence(value: &[u8]) -> Result<()> {
    let largest_rank = Rank::from(u64::MAX);
    let state = RegisterState {
        max_read_rank: largest_rank,
        max_write_rank: largest_rank,
        value: Some(value.to_vec()),
    };
    encode_state(&state).map(|_| ())
}

// ============================================================================
// CORE OPERATIONS (Paper Section 5.1, Figure 3)
// ============================================================================

/// Perform a ranked read on the register
///
/// Updates `max_read_rank` if the given rank is higher than the current one,
/// effectively installing a fence that prevents lower-ranked writes.
/// Returns the current write rank and value.
///
/// This is used by the leader before writing to ensure no concurrent
/// higher-ranked process has written.
pub async fn read<T>(txn: &T, subspace: &Subspace, rank: Rank) -> Result<ReadResult>
where
    T: Deref<Target = Transaction>,
{
    let key = keys::state_key(subspace);
    let mut state = read_state(txn, &key).await?;

    // Update max_read_rank if our rank is higher
    if rank > state.max_read_rank {
        #[cfg(feature = "trace")]
        tracing::debug!(
            register_action = "fence_installed",
            rank = rank.as_u64(),
            previous_max_read_rank = state.max_read_rank.as_u64(),
            "ranked-register fence staged in transaction"
        );
        state.max_read_rank = rank;
        write_state(txn, &key, &state)?;
    }

    Ok(ReadResult {
        write_rank: state.max_write_rank,
        value: state.value,
    })
}

/// Perform a ranked write on the register
///
/// Commits the value only if:
/// - `rank >= max_read_rank` (no higher-ranked read has installed a fence)
/// - `rank > max_write_rank` (no equal-or-higher-ranked write has occurred)
///
/// Returns `Committed` if the write succeeds, `Aborted` otherwise.
pub async fn write<T>(txn: &T, subspace: &Subspace, rank: Rank, value: &[u8]) -> Result<WriteResult>
where
    T: Deref<Target = Transaction>,
{
    let key = keys::state_key(subspace);
    let mut state = read_state(txn, &key).await?;

    // Check rank conditions (paper Section 5.1)
    if rank < state.max_read_rank || rank <= state.max_write_rank {
        #[cfg(feature = "trace")]
        tracing::debug!(
            register_action = "write_aborted",
            rank = rank.as_u64(),
            max_read_rank = state.max_read_rank.as_u64(),
            max_write_rank = state.max_write_rank.as_u64(),
            fenced_by_read = rank < state.max_read_rank,
            fenced_by_write = rank <= state.max_write_rank,
            "ranked-register write rejected by fencing rank"
        );
        return Ok(WriteResult::Aborted);
    }

    ensure_value_survives_future_fence(value)?;

    // Commit the write
    state.max_write_rank = rank;
    state.value = Some(value.to_vec());
    write_state(txn, &key, &state)?;

    Ok(WriteResult::Committed)
}

/// Read the current value without updating ranks
///
/// This is a plain read for followers and observers. It does not update
/// `max_read_rank`, so it installs no durable fence. As a non-snapshot
/// FoundationDB read, it still adds a conflict range for this register key.
///
/// Safe to call from any process at any time.
pub async fn value<T>(txn: &T, subspace: &Subspace) -> Result<Option<Vec<u8>>>
where
    T: Deref<Target = Transaction>,
{
    let key = keys::state_key(subspace);
    let state = read_state(txn, &key).await?;
    Ok(state.value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RetryDecision, RetryableError};

    fn state_with_value(value: Vec<u8>) -> RegisterState {
        RegisterState {
            max_read_rank: Rank::from(1_u64),
            max_write_rank: Rank::from(1_u64),
            value: Some(value),
        }
    }

    #[test]
    fn near_limit_plain_payload_fits_after_encoding() {
        let state = state_with_value(vec![b'x'; MAX_ENCODED_REGISTER_STATE_BYTES - 100]);

        let encoded = encode_state(&state).expect("plain payload must fit below the encoded cap");
        assert!(encoded.len() <= MAX_ENCODED_REGISTER_STATE_BYTES);
    }

    #[test]
    fn nul_heavy_payload_is_rejected_after_encoding() {
        let payload = vec![0; MAX_ENCODED_REGISTER_STATE_BYTES - 1];
        let error = encode_state(&state_with_value(payload))
            .expect_err("NUL escaping must push the encoded state over the cap");

        assert!(matches!(
            &error,
            RankedRegisterError::EncodedStateTooLarge {
                encoded_size,
                limit,
            } if *encoded_size > MAX_ENCODED_REGISTER_STATE_BYTES
                && *limit == MAX_ENCODED_REGISTER_STATE_BYTES
        ));
        assert!(matches!(error.retry_decision(), RetryDecision::Fatal));
    }

    #[test]
    fn near_cap_value_reserves_future_fence_headroom() {
        let largest_rank = Rank::from(u64::MAX);
        let max_rank_empty_state = RegisterState {
            max_read_rank: largest_rank,
            max_write_rank: largest_rank,
            value: Some(Vec::new()),
        };
        let payload = vec![
            b'x';
            MAX_ENCODED_REGISTER_STATE_BYTES
                - encode_state(&max_rank_empty_state)
                    .expect("empty max-rank state must fit")
                    .len()
                + 1
        ];
        let low_rank_state = RegisterState {
            max_read_rank: Rank::from(1_u64),
            max_write_rank: Rank::from(1_u64),
            value: Some(payload.clone()),
        };

        assert!(encode_state(&low_rank_state).is_ok());
        assert!(matches!(
            ensure_value_survives_future_fence(&payload),
            Err(RankedRegisterError::EncodedStateTooLarge { .. })
        ));
    }
}
