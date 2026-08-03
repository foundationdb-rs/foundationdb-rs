// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Error types for leader election.

use crate::FdbError;
use crate::tuple::PackError;
use std::fmt;

/// Leader-election-specific errors.
#[derive(Debug)]
pub enum LeaderElectionError {
    /// The durable state could not be decoded or violated an invariant.
    InvalidState(String),
    /// A participant ID was empty.
    InvalidParticipantId,
    /// A configured lease duration was zero.
    InvalidLeaseDuration,
    /// The durable revision reached `u64::MAX`.
    RevisionExhausted,
    /// An underlying FoundationDB error occurred.
    Fdb(FdbError),
    /// Failed to pack or unpack a tuple.
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

/// Result type for leader-election operations.
pub type Result<T> = std::result::Result<T, LeaderElectionError>;
