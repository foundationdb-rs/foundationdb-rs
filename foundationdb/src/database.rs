// Copyright 2018 foundationdb-rs developers, https://github.com/Clikengo/foundationdb-rs/graphs/contributors
// Copyright 2013-2018 Apple, Inc and the FoundationDB project authors.
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Implementations of the FDBDatabase C API
//!
//! <https://apple.github.io/foundationdb/api-c.html#database>

use std::convert::TryInto;
use std::marker::PhantomData;
use std::pin::Pin;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use foundationdb_macros::cfg_api_versions;
use foundationdb_sys as fdb_sys;

use crate::options;
use crate::transaction::*;
use crate::{error, FdbError, FdbResult};

use futures::prelude::*;

/// Represents a FoundationDB database
///
/// A mutable, lexicographically ordered mapping from binary keys to binary values.
///
/// Modifications to a database are performed via transactions.
pub struct Database {
    pub(crate) inner: NonNull<fdb_sys::FDBDatabase>,
}
unsafe impl Send for Database {}
unsafe impl Sync for Database {}
impl Drop for Database {
    fn drop(&mut self) {
        unsafe {
            fdb_sys::fdb_database_destroy(self.inner.as_ptr());
        }
    }
}

#[cfg_api_versions(min = 610)]
impl Database {
    /// Create a database for the given configuration path if any, or the default one.
    pub fn new(path: Option<&str>) -> FdbResult<Database> {
        let path_str =
            path.map(|path| std::ffi::CString::new(path).expect("path to be convertible to CStr"));
        let path_ptr = path_str
            .as_ref()
            .map(|path| path.as_ptr())
            .unwrap_or(std::ptr::null());
        let mut v: *mut fdb_sys::FDBDatabase = std::ptr::null_mut();
        let err = unsafe { fdb_sys::fdb_create_database(path_ptr, &mut v) };
        drop(path_str); // path_str own the CString that we are getting the ptr from
        error::eval(err)?;
        Ok(Database {
            inner: NonNull::new(v)
                .expect("fdb_create_database to not return null if there is no error"),
        })
    }

    /// Create a database for the given configuration path
    pub fn from_path(path: &str) -> FdbResult<Database> {
        Self::new(Some(path))
    }

    /// Create a database for the default configuration path
    pub fn default() -> FdbResult<Database> {
        Self::new(None)
    }
}

impl Database {
    /// Create a database for the given configuration path
    ///
    /// This is a compatibility api. If you only use API version ≥ 610 you should
    /// use `Database::new`, `Database::from_path` or  `Database::default`.
    pub async fn new_compat(path: Option<&str>) -> FdbResult<Database> {
        #[cfg(any(feature = "fdb-5_1", feature = "fdb-5_2", feature = "fdb-6_0"))]
        {
            let cluster = crate::cluster::Cluster::new(path).await?;
            let database = cluster.create_database().await?;
            Ok(database)
        }

        #[cfg(not(any(feature = "fdb-5_1", feature = "fdb-5_2", feature = "fdb-6_0")))]
        {
            Database::new(path)
        }
    }

    /// Called to set an option an on `Database`.
    pub fn set_option(&self, opt: options::DatabaseOption) -> FdbResult<()> {
        unsafe { opt.apply(self.inner.as_ptr()) }
    }

    /// Creates a new transaction on the given database.
    pub fn create_trx(&self) -> FdbResult<Transaction> {
        let mut trx: *mut fdb_sys::FDBTransaction = std::ptr::null_mut();
        let err =
            unsafe { fdb_sys::fdb_database_create_transaction(self.inner.as_ptr(), &mut trx) };
        error::eval(err)?;
        Ok(Transaction::new(NonNull::new(trx).expect(
            "fdb_database_create_transaction to not return null if there is no error",
        )))
    }

    /// `transact` returns a future which retries on error. It tries to resolve a future created by
    /// caller-provided function `f` inside a retry loop, providing it with a newly created
    /// transaction. After caller-provided future resolves, the transaction will be committed
    /// automatically.
    ///
    /// # Warning
    ///
    /// It might retry indefinitely if the transaction is highly contentious. It is recommended to
    /// set `TransactionOption::RetryLimit` or `TransactionOption::SetTimeout` on the transaction
    /// if the task need to be guaranteed to finish.
    ///
    /// Once [Generic Associated Types](https://github.com/rust-lang/rfcs/blob/master/text/1598-generic_associated_types.md)
    /// lands in stable rust, the returned future of f won't need to be boxed anymore, also the
    /// lifetime limitations around f might be lowered.
    pub async fn transact<F>(&self, mut f: F, options: TransactOption) -> Result<F::Item, F::Error>
    where
        F: DatabaseTransact,
    {
        let is_idempotent = options.is_idempotent;
        let time_out = options.time_out.map(|d| Instant::now() + d);
        let retry_limit = options.retry_limit;
        let mut tries: u32 = 0;
        let mut trx = self.create_trx()?;
        let mut can_retry = move || {
            tries += 1;
            retry_limit.map(|limit| tries < limit).unwrap_or(true)
                && time_out.map(|t| Instant::now() < t).unwrap_or(true)
        };
        loop {
            let r = f.transact(trx).await;
            f = r.0;
            trx = r.1;
            trx = match r.2 {
                Ok(item) => match trx.commit().await {
                    Ok(_) => break Ok(item),
                    Err(e) => {
                        if (is_idempotent || !e.is_maybe_committed()) && can_retry() {
                            e.on_error().await?
                        } else {
                            break Err(F::Error::from(e.into()));
                        }
                    }
                },
                Err(user_err) => match user_err.try_into_fdb_error() {
                    Ok(e) => {
                        if (is_idempotent || !e.is_maybe_committed()) && can_retry() {
                            trx.on_error(e).await?
                        } else {
                            break Err(F::Error::from(e));
                        }
                    }
                    Err(user_err) => break Err(user_err),
                },
            };
        }
    }

    pub fn transact_boxed<'trx, F, D, T, E>(
        &'trx self,
        data: D,
        f: F,
        options: TransactOption,
    ) -> impl Future<Output = Result<T, E>> + Send + 'trx
    where
        for<'a> F: FnMut(
            &'a Transaction,
            &'a mut D,
        ) -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'a>>,
        E: TransactError,
        F: Send + 'trx,
        T: Send + 'trx,
        E: Send + 'trx,
        D: Send + 'trx,
    {
        self.transact(
            boxed::FnMutBoxed {
                f,
                d: data,
                m: PhantomData,
            },
            options,
        )
    }

    pub fn transact_boxed_local<'trx, F, D, T, E>(
        &'trx self,
        data: D,
        f: F,
        options: TransactOption,
    ) -> impl Future<Output = Result<T, E>> + 'trx
    where
        for<'a> F:
            FnMut(&'a Transaction, &'a mut D) -> Pin<Box<dyn Future<Output = Result<T, E>> + 'a>>,
        E: TransactError,
        F: 'trx,
        T: 'trx,
        E: 'trx,
        D: 'trx,
    {
        self.transact(
            boxed_local::FnMutBoxedLocal {
                f,
                d: data,
                m: PhantomData,
            },
            options,
        )
    }
}
pub trait DatabaseTransact: Sized {
    type Item;
    type Error: TransactError;
    type Future: Future<Output = (Self, Transaction, Result<Self::Item, Self::Error>)>;
    fn transact(self, trx: Transaction) -> Self::Future;
}

#[allow(clippy::needless_lifetimes)]
#[allow(clippy::type_complexity)]
mod boxed {
    use super::*;

    async fn boxed_data_fut<'t, F, T, E, D>(
        mut f: FnMutBoxed<'t, F, D>,
        trx: Transaction,
    ) -> (FnMutBoxed<'t, F, D>, Transaction, Result<T, E>)
    where
        F: for<'a> FnMut(
            &'a Transaction,
            &'a mut D,
        ) -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'a>>,
        E: TransactError,
    {
        let r = (f.f)(&trx, &mut f.d).await;
        (f, trx, r)
    }

    pub struct FnMutBoxed<'t, F, D> {
        pub f: F,
        pub d: D,
        pub m: PhantomData<&'t ()>,
    }
    impl<'t, F, T, E, D> DatabaseTransact for FnMutBoxed<'t, F, D>
    where
        F: for<'a> FnMut(
            &'a Transaction,
            &'a mut D,
        ) -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'a>>,
        F: 't + Send,
        T: 't,
        E: 't,
        D: 't + Send,
        E: TransactError,
    {
        type Item = T;
        type Error = E;
        type Future = Pin<
            Box<
                dyn Future<Output = (Self, Transaction, Result<Self::Item, Self::Error>)>
                    + Send
                    + 't,
            >,
        >;

        fn transact(self, trx: Transaction) -> Self::Future {
            boxed_data_fut(self, trx).boxed()
        }
    }
}

#[allow(clippy::needless_lifetimes)]
#[allow(clippy::type_complexity)]
mod boxed_local {
    use super::*;

    async fn boxed_local_data_fut<'t, F, T, E, D>(
        mut f: FnMutBoxedLocal<'t, F, D>,
        trx: Transaction,
    ) -> (FnMutBoxedLocal<'t, F, D>, Transaction, Result<T, E>)
    where
        F: for<'a> FnMut(
            &'a Transaction,
            &'a mut D,
        ) -> Pin<Box<dyn Future<Output = Result<T, E>> + 'a>>,
        E: TransactError,
    {
        let r = (f.f)(&trx, &mut f.d).await;
        (f, trx, r)
    }

    pub struct FnMutBoxedLocal<'t, F, D> {
        pub f: F,
        pub d: D,
        pub m: PhantomData<&'t ()>,
    }
    impl<'t, F, T, E, D> DatabaseTransact for FnMutBoxedLocal<'t, F, D>
    where
        F: for<'a> FnMut(
            &'a Transaction,
            &'a mut D,
        ) -> Pin<Box<dyn Future<Output = Result<T, E>> + 'a>>,
        F: 't,
        T: 't,
        E: 't,
        D: 't,
        E: TransactError,
    {
        type Item = T;
        type Error = E;
        type Future = Pin<
            Box<dyn Future<Output = (Self, Transaction, Result<Self::Item, Self::Error>)> + 't>,
        >;

        fn transact(self, trx: Transaction) -> Self::Future {
            boxed_local_data_fut(self, trx).boxed_local()
        }
    }
}

/// A trait that must be implemented to use `Database::transact` this application error types.
pub trait TransactError: From<FdbError> {
    fn try_into_fdb_error(self) -> Result<FdbError, Self>;
}
impl<T> TransactError for T
where
    T: From<FdbError> + TryInto<FdbError, Error = T>,
{
    fn try_into_fdb_error(self) -> Result<FdbError, Self> {
        self.try_into()
    }
}
impl TransactError for FdbError {
    fn try_into_fdb_error(self) -> Result<FdbError, Self> {
        Ok(self)
    }
}

/// A set of options that controls the behavior of `Database::transact`.
#[derive(Default, Clone)]
pub struct TransactOption {
    pub retry_limit: Option<u32>,
    pub time_out: Option<Duration>,
    pub is_idempotent: bool,
}

impl TransactOption {
    /// An idempotent TransactOption
    pub fn idempotent() -> Self {
        Self {
            is_idempotent: true,
            ..TransactOption::default()
        }
    }
}
