// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Error types for leader election
//!
//! Defines the error hierarchy for leader election operations,
//! including election-specific errors and conversions from underlying errors.

use crate::tuple::PackError;
use crate::FdbError;
use std::fmt;

/// Leader election specific errors
///
/// Represents all possible error conditions that can occur during
/// leader election operations.
#[derive(Debug)]
pub enum LeaderElectionError {
    /// Election is currently disabled
    ///
    /// Returned when attempting election operations while the election
    /// system is administratively disabled
    ElectionDisabled,
    /// Process not found
    ///
    /// The specified process UUID doesn't exist in the election registry
    ProcessNotFound(String),
    /// Global configuration not initialized
    ///
    /// The election system hasn't been initialized with `initialize()`
    NotInitialized,
    /// Invalid state
    ///
    /// The election system is in an inconsistent or unexpected state
    InvalidState(String),
    /// Database error
    ///
    /// An underlying FoundationDB error occurred
    Fdb(FdbError),
    /// Serialization error
    ///
    /// Failed to pack/unpack data using the tuple layer
    PackError(PackError),
    /// Candidate not registered
    ///
    /// The process attempted to claim leadership without being registered as a candidate
    UnregisteredCandidate,
}

impl fmt::Display for LeaderElectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ElectionDisabled => write!(f, "Election is currently disabled"),
            Self::ProcessNotFound(id) => write!(f, "Process not found: {id}"),
            Self::NotInitialized => write!(f, "Global configuration not initialized"),
            Self::InvalidState(msg) => write!(f, "Invalid state: {msg}"),
            Self::Fdb(e) => write!(f, "Database error: {e}"),
            Self::PackError(e) => write!(f, "Pack error: {e:?}"),
            Self::UnregisteredCandidate => write!(f, "Candidate not registered"),
        }
    }
}

impl std::error::Error for LeaderElectionError {}

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

/// Result type for leader election operations
///
/// Convenience type alias for Results that may fail with LeaderElectionError
pub type Result<T> = std::result::Result<T, LeaderElectionError>;
