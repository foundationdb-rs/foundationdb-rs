// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Key layout and record encoding for leader election
//!
//! # Keys
//!
//! ```text
//! <subspace>/leader                 - the contested record (reads, CAS writes)
//! <subspace>/term                   - bumped only on claim/steal/resign; watches park here
//! <subspace>/history/<versionstamp> - audit trail of transitions, bounded retention
//! ```
//!
//! Splitting the term key out of the leader key is what keeps a renewal from
//! waking every contender: renewals rewrite the leader record only, so a watch
//! on the term key fires on real leadership changes and nothing else.
//!
//! # Record value
//!
//! `(SCHEMA_VERSION, ballot, generation, leader_id, token, lease_nanos)`
//!
//! Decoding is strict. A truncated value, an unknown schema version, or a
//! combination of fields that is neither fully occupied nor fully vacant is a
//! [`LeaderElectionError::CorruptRecord`], never a silent misread. Records
//! written by the previous version of this recipe fail here by design.

use super::errors::{LeaderElectionError, Result};
use super::types::*;
use crate::tuple::{Subspace, Versionstamp, pack, unpack};

/// Key prefixes within the election subspace
pub(crate) const LEADER_PREFIX: &str = "leader";
pub(crate) const TERM_PREFIX: &str = "term";
pub(crate) const HISTORY_PREFIX: &str = "history";

// ============================================================================
// KEYS
// ============================================================================

/// The contested leader record key
pub(crate) fn leader_key(subspace: &Subspace) -> Vec<u8> {
    subspace.pack(&(LEADER_PREFIX,))
}

/// The key watches park on
pub(crate) fn term_key(subspace: &Subspace) -> Vec<u8> {
    subspace.pack(&(TERM_PREFIX,))
}

/// The subspace holding the transition audit trail
pub(crate) fn history_subspace(subspace: &Subspace) -> Subspace {
    subspace.subspace(&(HISTORY_PREFIX,))
}

/// A history key with an unresolved versionstamp
///
/// Must be written with [`MutationType::SetVersionstampedKey`], which fills the
/// versionstamp in at commit time so events order exactly as they committed.
///
/// [`MutationType::SetVersionstampedKey`]: crate::options::MutationType::SetVersionstampedKey
pub(crate) fn incomplete_history_key(subspace: &Subspace) -> Vec<u8> {
    history_subspace(subspace).pack_with_versionstamp(&(Versionstamp::incomplete(0),))
}

// ============================================================================
// LEADER RECORD
// ============================================================================

fn corrupt(msg: impl Into<String>) -> LeaderElectionError {
    LeaderElectionError::CorruptRecord(msg.into())
}

/// Encode a leader record for storage
pub(crate) fn encode_record(record: &LeaderRecord) -> Vec<u8> {
    pack(&(
        SCHEMA_VERSION,
        record.ballot,
        record.generation,
        record.leader_id.as_str(),
        record.token.as_bytes().as_slice(),
        record.lease_nanos,
    ))
}

/// Decode a stored leader record
///
/// # Errors
///
/// [`LeaderElectionError::CorruptRecord`] on anything that is not a
/// well-formed record of a known schema version.
pub(crate) fn decode_record(bytes: &[u8]) -> Result<LeaderRecord> {
    let (version, ballot, generation, leader_id, token, lease_nanos): (
        u64,
        u64,
        u64,
        String,
        Vec<u8>,
        u64,
    ) = unpack(bytes).map_err(|e| corrupt(format!("value is not a leader record tuple: {e:?}")))?;

    if version != SCHEMA_VERSION {
        return Err(corrupt(format!(
            "unknown schema version {version}, this build understands {SCHEMA_VERSION}"
        )));
    }
    if ballot == 0 {
        return Err(corrupt("ballot 0 is not a valid term"));
    }
    let token: [u8; 16] = token
        .try_into()
        .map_err(|_| corrupt("token is not 16 bytes"))?;
    let token = ClaimToken::from_bytes(token);

    // Occupied and vacant are the only two consistent shapes; anything in
    // between means the record was written by something that is not this
    // protocol.
    let occupied = (!leader_id.is_empty(), !token.is_zero(), lease_nanos != 0);
    match occupied {
        (true, true, true) | (false, false, false) => {}
        _ => {
            return Err(corrupt("record is neither fully occupied nor fully vacant"));
        }
    }

    Ok(LeaderRecord {
        ballot,
        generation,
        leader_id,
        token,
        lease_nanos,
    })
}

/// Build the record a claim or steal writes
pub(crate) fn claimed_record(
    ballot: u64,
    generation: u64,
    leader_id: &str,
    token: ClaimToken,
    lease: LeaseDuration,
) -> LeaderRecord {
    LeaderRecord {
        ballot,
        generation,
        leader_id: leader_id.to_string(),
        token,
        lease_nanos: lease.as_nanos(),
    }
}

/// Build the record a resign writes
///
/// The ballot and generation are preserved so that the successor lands at
/// `ballot + 1` and observers see the identity change.
pub(crate) fn vacant_record(ballot: u64, generation: u64) -> LeaderRecord {
    LeaderRecord {
        ballot,
        generation,
        leader_id: String::new(),
        token: ClaimToken::ZERO,
        lease_nanos: 0,
    }
}

// ============================================================================
// TERM MARKER
// ============================================================================

/// Encode the term marker
///
/// Includes the occupancy flag so that a resign, which preserves both ballot
/// and generation, still changes the value and therefore still fires watches.
pub(crate) fn encode_term(record: &LeaderRecord) -> Vec<u8> {
    pack(&(record.ballot, record.generation, !record.is_vacant()))
}

// ============================================================================
// HISTORY
// ============================================================================

/// Encode a history entry
pub(crate) fn encode_history(kind: HistoryEventKind, ballot: u64, leader_id: &str) -> Vec<u8> {
    pack(&(kind.as_str(), ballot, leader_id))
}

/// Decode a history entry from its key and value
pub(crate) fn decode_history(
    subspace: &Subspace,
    key: &[u8],
    value: &[u8],
) -> Result<HistoryEvent> {
    let (versionstamp,): (Versionstamp,) = history_subspace(subspace)
        .unpack(key)
        .map_err(|e| corrupt(format!("history key is not versionstamped: {e:?}")))?;

    let (kind, ballot, leader_id): (String, u64, String) =
        unpack(value).map_err(|e| corrupt(format!("history value is not an event: {e:?}")))?;

    let kind = HistoryEventKind::from_str(&kind)
        .ok_or_else(|| corrupt(format!("unknown history event kind {kind:?}")))?;

    Ok(HistoryEvent {
        versionstamp: *versionstamp.as_bytes(),
        kind,
        ballot,
        leader_id,
    })
}
