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
    tuple::{pack, unpack, Subspace},
    Transaction,
};
use std::ops::Deref;

use super::{errors::Result, keys, types::*};

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
fn write_state<T>(txn: &T, key: &[u8], state: &RegisterState)
where
    T: Deref<Target = Transaction>,
{
    let has_value = state.value.is_some();
    let value = state.value.as_deref().unwrap_or(&[]);

    let data = (
        state.max_read_rank.as_u64(),
        state.max_write_rank.as_u64(),
        has_value,
        value,
    );
    let packed = pack(&data);
    txn.set(key, &packed);
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
        state.max_read_rank = rank;
        write_state(txn, &key, &state);
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
        return Ok(WriteResult::Aborted);
    }

    // Commit the write
    state.max_write_rank = rank;
    state.value = Some(value.to_vec());
    write_state(txn, &key, &state);

    Ok(WriteResult::Committed)
}

/// Read the current value without updating ranks
///
/// This is a plain read for followers/observers. It does NOT update
/// `max_read_rank`, so it won't interfere with the leader's writes.
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
