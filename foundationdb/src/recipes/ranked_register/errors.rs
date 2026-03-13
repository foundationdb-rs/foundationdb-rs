// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Error types for the ranked register

use crate::tuple::PackError;
use crate::FdbError;
use std::fmt;

/// Ranked register specific errors
#[derive(Debug)]
pub enum RankedRegisterError {
    /// Database error
    Fdb(FdbError),
    /// Serialization error
    PackError(PackError),
    /// Invalid state encountered in the register
    InvalidState(String),
}

impl fmt::Display for RankedRegisterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fdb(e) => write!(f, "Database error: {e}"),
            Self::PackError(e) => write!(f, "Pack error: {e:?}"),
            Self::InvalidState(msg) => write!(f, "Invalid state: {msg}"),
        }
    }
}

impl std::error::Error for RankedRegisterError {}

impl From<FdbError> for RankedRegisterError {
    fn from(error: FdbError) -> Self {
        Self::Fdb(error)
    }
}

impl From<PackError> for RankedRegisterError {
    fn from(error: PackError) -> Self {
        Self::PackError(error)
    }
}

/// Result type for ranked register operations
pub type Result<T> = std::result::Result<T, RankedRegisterError>;
