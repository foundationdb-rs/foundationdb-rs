// Copyright 2018 foundationdb-rs developers, https://github.com/Clikengo/foundationdb-rs/graphs/contributors
// Copyright 2013-2018 Apple, Inc and the FoundationDB project authors.
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Error types for the Fdb crate

use std::ffi::CStr;
use std::fmt;
use std::fmt::{Debug, Display, Formatter};

use crate::directory::DirectoryError;
use crate::options;
use crate::tuple::hca::HcaError;
use crate::tuple::PackError;
use foundationdb_sys as fdb_sys;

pub(crate) fn eval(error_code: fdb_sys::fdb_error_t) -> FdbResult<()> {
    let rust_code: i32 = error_code;
    if rust_code == 0 {
        Ok(())
    } else {
        Err(FdbError::from_code(error_code))
    }
}

/// The Standard Error type of FoundationDB
#[derive(Debug, Copy, Clone)]
pub struct FdbError {
    /// The FoundationDB error code
    error_code: i32,
}

impl FdbError {
    /// Converts from a raw foundationDB error code
    pub fn from_code(error_code: fdb_sys::fdb_error_t) -> Self {
        Self { error_code }
    }

    pub(crate) fn new(error_code: i32) -> Self {
        Self { error_code }
    }

    pub fn message(self) -> &'static str {
        let error_str =
            unsafe { CStr::from_ptr::<'static>(fdb_sys::fdb_get_error(self.error_code)) };
        error_str
            .to_str()
            .expect("bad error string from FoundationDB")
    }

    fn is_error_predicate(self, predicate: options::ErrorPredicate) -> bool {
        // This cast to `i32` isn't unnecessary in all configurations.
        #[allow(clippy::unnecessary_cast)]
        let check =
            unsafe { fdb_sys::fdb_error_predicate(predicate.code() as i32, self.error_code) };

        check != 0
    }

    /// Indicates the transaction may have succeeded, though not in a way the system can verify.
    pub fn is_maybe_committed(self) -> bool {
        self.is_error_predicate(options::ErrorPredicate::MaybeCommitted)
    }

    /// Indicates the operations in the transactions should be retried because of transient error.
    pub fn is_retryable(self) -> bool {
        self.is_error_predicate(options::ErrorPredicate::Retryable)
    }

    /// Indicates the transaction has not committed, though in a way that can be retried.
    pub fn is_retryable_not_committed(self) -> bool {
        self.is_error_predicate(options::ErrorPredicate::RetryableNotCommitted)
    }

    /// Raw foundationdb error code
    pub fn code(self) -> i32 {
        self.error_code
    }
}

impl fmt::Display for FdbError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        std::fmt::Display::fmt(&self.message(), f)
    }
}

impl std::error::Error for FdbError {}

/// Alias for `Result<..., FdbError>`
pub type FdbResult<T = ()> = Result<T, FdbError>;

/// This error represent all errors that can be throwed by `db.run`.
/// Layer developers may use the `CustomError`.
pub enum FdbBindingError {
    NonRetryableFdbError(FdbError),
    HcaError(HcaError),
    DirectoryError(DirectoryError),
    PackError(PackError),
    /// A reference to the `RetryableTransaction` has been kept
    ReferenceToTransactionKept,
    /// A custom error that layer developers can use
    CustomError(Box<dyn std::error::Error + Send + Sync>),
}

impl FdbBindingError {
    /// Returns the underlying `FdbError`, if any.
    pub fn get_fdb_error(&self) -> Option<FdbError> {
        match *self {
            Self::NonRetryableFdbError(error)
            | Self::DirectoryError(DirectoryError::FdbError(error))
            | Self::HcaError(HcaError::FdbError(error)) => Some(error),
            Self::CustomError(ref error) => {
                if let Some(e) = error.downcast_ref::<FdbError>() {
                    Some(*e)
                } else if let Some(e) = error.downcast_ref::<FdbBindingError>() {
                    e.get_fdb_error()
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

impl From<FdbError> for FdbBindingError {
    fn from(e: FdbError) -> Self {
        Self::NonRetryableFdbError(e)
    }
}

impl From<HcaError> for FdbBindingError {
    fn from(e: HcaError) -> Self {
        Self::HcaError(e)
    }
}

impl From<DirectoryError> for FdbBindingError {
    fn from(e: DirectoryError) -> Self {
        Self::DirectoryError(e)
    }
}

impl FdbBindingError {
    /// create a new custom error
    pub fn new_custom_error(e: Box<dyn std::error::Error + Send + Sync>) -> Self {
        Self::CustomError(e)
    }
}

impl Debug for FdbBindingError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            FdbBindingError::NonRetryableFdbError(err) => write!(f, "{:?}", err),
            FdbBindingError::HcaError(err) => write!(f, "{:?}", err),
            FdbBindingError::DirectoryError(err) => write!(f, "{:?}", err),
            FdbBindingError::PackError(err) => write!(f, "{:?}", err),
            FdbBindingError::ReferenceToTransactionKept => {
                write!(f, "Reference to transaction kept")
            }
            FdbBindingError::CustomError(err) => write!(f, "{:?}", err),
        }
    }
}

impl Display for FdbBindingError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        std::fmt::Debug::fmt(&self, f)
    }
}

impl std::error::Error for FdbBindingError {}
