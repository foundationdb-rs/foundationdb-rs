// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Error types for leader election

use crate::tuple::PackError;
use crate::{FdbBindingError, FdbError, RetryableError};
use std::fmt;

/// Leader election specific errors
///
/// The `Binding` payload is boxed so that this type stays small and does not
/// form a cycle with [`FdbBindingError`]: the retry loop of
/// [`crate::Database::run`] recovers the underlying [`FdbError`] by walking the
/// `source()` chain, which this type keeps intact.
#[derive(Debug)]
#[non_exhaustive]
pub enum LeaderElectionError {
    /// Database error
    Fdb(FdbError),
    /// Retry loop error
    Binding(Box<FdbBindingError>),
    /// Serialization error
    Pack(PackError),
    /// The stored record could not be decoded as a leader record
    ///
    /// Raised on a truncated value, an unknown schema version, or a record
    /// whose fields contradict each other. Whatever wrote those bytes, they
    /// are not a record this build understands, and decoding fails loudly
    /// rather than guessing at what they might have meant.
    CorruptRecord(String),
    /// The ballot space is exhausted
    ///
    /// Ballots are capped at [`u32::MAX`] so that `LeaseGrant::rank` stays
    /// infallible. Practically unreachable: it takes more than four billion
    /// leadership changes on a single election subspace.
    BallotExhausted,
    /// The per-term fencing sequence space is exhausted
    ///
    /// Returned before a `u32` rank sequence would wrap, which would let a
    /// stale rank compare as fresh.
    RankExhausted,
    /// A leadership token was used after the term ended
    ///
    /// Carries the same information as [`LeaseLostError`], which is what the
    /// handle's own staleness checks return; this variant is how it reaches
    /// operations that can also fail for other reasons.
    LeaseLost(LeaseLostError),
    /// A configuration value is not usable
    InvalidConfig(String),
    /// An argument failed validation
    InvalidArgument(String),
}

impl fmt::Display for LeaderElectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fdb(e) => write!(f, "Database error: {e}"),
            Self::Binding(e) => write!(f, "Retry loop error: {e}"),
            Self::Pack(e) => write!(f, "Pack error: {e:?}"),
            Self::CorruptRecord(msg) => write!(f, "Corrupt leader record: {msg}"),
            Self::BallotExhausted => write!(f, "Ballot space exhausted"),
            Self::RankExhausted => write!(f, "Fencing sequence space exhausted"),
            Self::LeaseLost(e) => write!(f, "{e}"),
            Self::InvalidConfig(msg) => write!(f, "Invalid configuration: {msg}"),
            Self::InvalidArgument(msg) => write!(f, "Invalid argument: {msg}"),
        }
    }
}

impl std::error::Error for LeaderElectionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Fdb(e) => Some(e),
            Self::Binding(e) => Some(e.as_ref()),
            Self::Pack(e) => Some(e),
            Self::LeaseLost(e) => Some(e),
            _ => None,
        }
    }
}

impl From<FdbError> for LeaderElectionError {
    fn from(error: FdbError) -> Self {
        Self::Fdb(error)
    }
}

impl From<FdbBindingError> for LeaderElectionError {
    fn from(error: FdbBindingError) -> Self {
        Self::Binding(Box::new(error))
    }
}

impl From<LeaseLostError> for LeaderElectionError {
    fn from(error: LeaseLostError) -> Self {
        Self::LeaseLost(error)
    }
}

impl From<PackError> for LeaderElectionError {
    fn from(error: PackError) -> Self {
        Self::Pack(error)
    }
}

/// The `source()` chain exposes the wrapped `FdbError`, so the default
/// `retry_decision` makes this error retry-transparent in `db.run`.
impl RetryableError for LeaderElectionError {}

/// Signals that a leadership token can no longer be trusted
///
/// Returned by the staleness checks on the handle layer. Distinct from
/// [`LeaderElectionError`] because losing a lease is an expected outcome, not
/// a failure of the election machinery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseLostError {
    /// The ballot of the term that was lost
    pub ballot: u64,
}

impl fmt::Display for LeaseLostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "leadership lease lost (ballot {})", self.ballot)
    }
}

impl std::error::Error for LeaseLostError {}

/// Result type for leader election operations
pub type Result<T> = std::result::Result<T, LeaderElectionError>;
