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

use crate::options;
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

    pub fn message(self) -> &'static str {
        let error_str =
            unsafe { CStr::from_ptr::<'static>(fdb_sys::fdb_get_error(self.error_code)) };
        error_str
            .to_str()
            .expect("bad error string from FoundationDB")
    }

    fn is_error_predicate(self, predicate: options::ErrorPredicate) -> bool {
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
        self.message().fmt(f)
    }
}

impl std::error::Error for FdbError {}

/// Alias for `Result<..., FdbError>`
pub type FdbResult<T> = Result<T, FdbError>;
