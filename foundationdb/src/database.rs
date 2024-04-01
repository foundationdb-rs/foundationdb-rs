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

use crate::error::FdbBindingError;
use futures::prelude::*;

#[cfg(any(feature = "fdb-7_1", feature = "fdb-7_3"))]
#[cfg(feature = "tenant-experimental")]
use crate::tenant::FdbTenant;

/// Wrapper around the boolean representing whether the
/// previous transaction is still on fly
/// This wrapper prevents the boolean to be copy and force it
/// to be moved instead.
/// This pretty handy when you don't want to see the `Database::run` closure
/// capturing the environment.
pub struct MaybeCommitted(bool);

impl From<MaybeCommitted> for bool {
    fn from(value: MaybeCommitted) -> Self {
        value.0
    }
}

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
        let ptr =
            NonNull::new(v).expect("fdb_create_database to not return null if there is no error");
        Ok(Self::new_from_pointer(ptr))
    }

    /// Create a new FDBDatabase from a raw pointer. Users are expected to use the `new` method.
    pub fn new_from_pointer(ptr: NonNull<fdb_sys::FDBDatabase>) -> Self {
        Self { inner: ptr }
    }

    /// Create a database for the given configuration path
    pub fn from_path(path: &str) -> FdbResult<Database> {
        Self::new(Some(path))
    }

    /// Create a database for the default configuration path
    #[allow(clippy::should_implement_trait)]
    pub fn default() -> FdbResult<Database> {
        Self::new(None)
    }
}

#[cfg_api_versions(min = 710)]
#[cfg(feature = "tenant-experimental")]
impl Database {
    pub fn open_tenant(&self, tenant_name: &[u8]) -> FdbResult<FdbTenant> {
        let mut ptr: *mut fdb_sys::FDB_tenant = std::ptr::null_mut();
        let err = unsafe {
            fdb_sys::fdb_database_open_tenant(
                self.inner.as_ptr(),
                tenant_name.as_ptr(),
                tenant_name.len().try_into().unwrap(),
                &mut ptr,
            )
        };
        error::eval(err)?;
        Ok(FdbTenant {
            inner: NonNull::new(ptr)
                .expect("fdb_database_open_tenant to not return null if there is no error"),
            name: tenant_name.to_owned(),
        })
    }
}

#[cfg_api_versions(min = 730)]
impl Database {
    /// Retrieve a client-side status information in a JSON format.
    pub fn get_client_status(
        &self,
    ) -> impl Future<Output = FdbResult<crate::future::FdbSlice>> + Send + Sync + Unpin {
        crate::future::FdbFuture::new(unsafe {
            fdb_sys::fdb_database_get_client_status(self.inner.as_ptr())
        })
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

    fn create_retryable_trx(&self) -> FdbResult<RetryableTransaction> {
        Ok(RetryableTransaction::new(self.create_trx()?))
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

    /// Runs a transactional function against this Database with retry logic.
    /// The associated closure will be called until a non-retryable FDBError
    /// is thrown or commit(), returns success.
    ///
    /// Users are **not** expected to keep reference to the `RetryableTransaction`. If a weak or strong
    /// reference is kept by the user, the binding will throw an error.
    ///
    /// # Warning: retry
    ///
    /// It might retry indefinitely if the transaction is highly contentious. It is recommended to
    /// set [`options::TransactionOption::RetryLimit`] or [`options::TransactionOption::Timeout`] on the transaction
    /// if the task need to be guaranteed to finish. These options can be safely set on every iteration of the closure.
    ///
    /// # Warning: Maybe committed transactions
    ///
    /// As with other client/server databases, in some failure scenarios a client may be unable to determine
    /// whether a transaction succeeded. You should make sure your closure is idempotent.
    ///
    /// The closure will notify the user in case of a maybe_committed transaction in a previous run
    ///  with the `MaybeCommitted` provided in the closure.
    ///
    /// This one can used as boolean with
    /// ```ignore
    /// db.run(|trx, maybe_committed| async {
    ///     if maybe_committed.into() {
    ///         // Handle the problem if needed
    ///     }
    /// }).await;
    ///```
    pub async fn run<F, Fut, T>(&self, closure: F) -> Result<T, FdbBindingError>
    where
        F: Fn(RetryableTransaction, MaybeCommitted) -> Fut,
        Fut: Future<Output = Result<T, FdbBindingError>>,
    {
        let mut maybe_committed_transaction = false;
        // we just need to create the transaction once,
        // in case there is a error, it will be reset automatically
        let mut transaction = self.create_retryable_trx()?;

        loop {
            // executing the closure
            let result_closure = closure(
                transaction.clone(),
                MaybeCommitted(maybe_committed_transaction),
            )
            .await;

            if let Err(e) = result_closure {
                // checks if it is an FdbError
                if let Some(e) = e.get_fdb_error() {
                    maybe_committed_transaction = e.is_maybe_committed();
                    // The closure returned an Error,
                    match transaction.on_error(e).await {
                        // we can retry the error
                        Ok(Ok(t)) => {
                            transaction = t;
                            continue;
                        }
                        Ok(Err(non_retryable_error)) => {
                            return Err(FdbBindingError::from(non_retryable_error))
                        }
                        // The only FdbBindingError that can be thrown here is `ReferenceToTransactionKept`
                        Err(non_retryable_error) => return Err(non_retryable_error),
                    }
                }
                // Otherwise, it cannot be retried
                return Err(e);
            }

            let commit_result = transaction.commit().await;

            match commit_result {
                // The only FdbBindingError that can be thrown here is `ReferenceToTransactionKept`
                Err(err) => return Err(err),
                Ok(Ok(_)) => return result_closure,
                Ok(Err(transaction_commit_error)) => {
                    maybe_committed_transaction = transaction_commit_error.is_maybe_committed();
                    // we have an error during commit, checking if it is a retryable error
                    match transaction_commit_error.on_error().await {
                        Ok(t) => {
                            transaction = RetryableTransaction::new(t);
                            continue;
                        }
                        Err(non_retryable_error) => {
                            return Err(FdbBindingError::from(non_retryable_error))
                        }
                    }
                }
            }
        }
    }

    /// Perform a no-op against FDB to check network thread liveness. This operation will not change the underlying data
    /// in any way, nor will it perform any I/O against the FDB cluster. However, it will schedule some amount of work
    /// onto the FDB client and wait for it to complete. The FoundationDB client operates by scheduling onto an event
    /// queue that is then processed by a single thread (the "network thread"). This method can be used to determine if
    /// the network thread has entered a state where it is no longer processing requests or if its time to process
    /// requests has increased. If the network thread is busy, this operation may take some amount of time to complete,
    /// which is why this operation returns a future.
    pub async fn perform_no_op(&self) -> FdbResult<()> {
        let trx = self.create_trx()?;

        // Set the read version of the transaction, then read it back. This requires no I/O, but it does
        // require the network thread be running. The exact value used for the read version is unimportant.
        trx.set_read_version(42);
        trx.get_read_version().await?;
        Ok(())
    }

    /// Returns a value where 0 indicates that the client is idle and 1 (or larger) indicates that the client is saturated.
    /// By default, this value is updated every second.
    #[cfg_api_versions(min = 710)]
    pub async fn get_main_thread_busyness(&self) -> FdbResult<f64> {
        let busyness =
            unsafe { fdb_sys::fdb_database_get_main_thread_busyness(self.inner.as_ptr()) };
        Ok(busyness)
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
