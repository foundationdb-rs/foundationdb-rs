// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Error types returned by leader-election operations.

use crate::FdbError;
use crate::tuple::PackError;
use std::fmt;

/// Leader-election-specific errors.
///
/// [`LeaderElectionError::Fdb`] retains the underlying FoundationDB error for
/// the caller's transaction runner to classify. The other variants identify
/// invalid local configuration, unsupported durable data, or a protocol limit.
#[derive(Debug)]
pub enum LeaderElectionError {
    /// The durable state could not be decoded or violated a protocol invariant.
    ///
    /// Do not treat this as an unowned state. It can indicate corrupt data, an
    /// incompatible schema, or data written outside this recipe.
    InvalidState(String),
    /// A [`ParticipantId`](super::ParticipantId) was empty.
    ///
    /// Empty text cannot distinguish a participating process incarnation.
    InvalidParticipantId,
    /// A configured lease duration was zero.
    ///
    /// A zero duration would make every local validity interval immediately
    /// expired, so [`super::LeaderElection::new`] rejects it.
    InvalidLeaseDuration,
    /// The durable revision reached `u64::MAX`.
    ///
    /// No further ownership transition can create a strictly newer fencing
    /// rank in this election subspace.
    RevisionExhausted,
    /// An underlying FoundationDB read, write, or commit-related error occurred.
    ///
    /// Inspect the source error and let the enclosing transaction runner apply
    /// its normal retry policy where appropriate.
    Fdb(FdbError),
    /// Failed to encode or decode the recipe's durable tuple state.
    ///
    /// On reads this can accompany malformed or incompatible durable data; on
    /// writes it prevents staging the requested state change.
    PackError(PackError),
}

impl fmt::Display for LeaderElectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidState(message) => write!(f, "Invalid leader election state: {message}"),
            Self::InvalidParticipantId => write!(f, "Participant ID must not be empty"),
            Self::InvalidLeaseDuration => write!(f, "Lease duration must not be zero"),
            Self::RevisionExhausted => write!(f, "Leader-election revision is exhausted"),
            Self::Fdb(error) => write!(f, "Database error: {error}"),
            Self::PackError(error) => write!(f, "Pack error: {error:?}"),
        }
    }
}

impl std::error::Error for LeaderElectionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Fdb(error) => Some(error),
            Self::PackError(error) => Some(error),
            Self::InvalidState(_)
            | Self::InvalidParticipantId
            | Self::InvalidLeaseDuration
            | Self::RevisionExhausted => None,
        }
    }
}

impl From<FdbError> for LeaderElectionError {
    fn from(error: FdbError) -> Self {
        Self::Fdb(error)
    }
}

impl From<PackError> for LeaderElectionError {
    fn from(error: PackError) -> Self {
        Self::PackError(error)
    }
}

/// Result type for [`super::LeaderElection`] operations.
pub type Result<T> = std::result::Result<T, LeaderElectionError>;
