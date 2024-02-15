//! Implementation of the Tenants API. Experimental features exposed through the `tenant-experimental` feature.
//!
//! Please refers to the official [documentation](https://apple.github.io/foundationdb/tenants.html)
//!
//! ## Warning
//!
//! Tenants are currently experimental and are not recommended for use in production. Please note that
//! currently, we [cannot upgrade a cluster with tenants enabled](https://forums.foundationdb.org/t/tenant-feature-metadata-changes-in-7-2-release/3459).

use crate::options::TransactionOption;
use std::future::Future;

use crate::database::TransactError;
use crate::{
    error, Database, DatabaseTransact, FdbBindingError, FdbError, FdbResult, KeySelector,
    RangeOption, RetryableTransaction, TransactOption, Transaction,
};
use foundationdb_sys as fdb_sys;
use futures::TryStreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Error;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

#[cfg(feature = "fdb-7_1")]
const TENANT_MAP_PREFIX: &[u8] = b"\xFF\xFF/management/tenant_map/";
#[cfg(feature = "fdb-7_1")]
const TENANT_MAP_PREFIX_END: &[u8] = b"\xFF\xFF/management/tenant_map0";

#[cfg(feature = "fdb-7_3")]
const TENANT_MAP_PREFIX: &[u8] = b"\xFF\xFF/management/tenant/map/";
#[cfg(feature = "fdb-7_3")]
const TENANT_MAP_PREFIX_END: &[u8] = b"\xFF\xFF/management/tenant/map0";

/// A `FdbTenant` represents a named key-space within a database that can be interacted with transactionally.
pub struct FdbTenant {
    pub(crate) inner: NonNull<fdb_sys::FDBTenant>,
    pub(crate) name: Vec<u8>,
}

unsafe impl Send for FdbTenant {}
unsafe impl Sync for FdbTenant {}

impl Drop for FdbTenant {
    fn drop(&mut self) {
        unsafe {
            fdb_sys::fdb_tenant_destroy(self.inner.as_ptr());
        }
    }
}

impl FdbTenant {
    /// Returns the name of this [`FdbTenant`].
    pub fn get_name(&self) -> &[u8] {
        &self.name
    }

    /// Creates a new transaction on the given database and tenant.
    pub fn create_trx(&self) -> FdbResult<Transaction> {
        let mut trx: *mut fdb_sys::FDBTransaction = std::ptr::null_mut();
        let err = unsafe { fdb_sys::fdb_tenant_create_transaction(self.inner.as_ptr(), &mut trx) };
        error::eval(err)?;
        Ok(Transaction::new(NonNull::new(trx).expect(
            "fdb_tenant_create_transaction to not return null if there is no error",
        )))
    }

    fn create_retryable_trx(&self) -> FdbResult<RetryableTransaction> {
        Ok(RetryableTransaction::new(self.create_trx()?))
    }

    /// Runs a transactional function against this Tenant with retry logic.
    /// The associated closure will be called until a non-retryable FDBError
    /// is thrown or commit(), returns success.
    ///
    /// Users are **not** expected to keep reference to the `RetryableTransaction`. If a weak or strong
    /// reference is kept by the user, the binding will throw an error.
    ///
    /// # Warning: retry
    ///
    /// It might retry indefinitely if the transaction is highly contentious. It is recommended to
    /// set the [crate::options::TransactionOption::RetryLimit] or [crate::options::TransactionOption::RetryLimit] on the transaction
    /// if the task need to be guaranteed to finish. These options can be safely set on every iteration of the closure.
    ///
    /// # Warning: Maybe committed transactions
    ///
    /// As with other client/server databases, in some failure scenarios a client may be unable to determine
    /// whether a transaction succeeded. You should make sure your closure is idempotent.
    ///
    /// The closure will notify the user in case of a maybe_committed transaction in a previous run
    ///  with the boolean provided in the closure.
    ///
    pub async fn run<F, Fut, T>(&self, closure: F) -> Result<T, FdbBindingError>
    where
        F: Fn(RetryableTransaction, bool) -> Fut,
        Fut: Future<Output = Result<T, FdbBindingError>>,
    {
        let mut maybe_committed_transaction = false;
        // we just need to create the transaction once,
        // in case there is a error, it will be reset automatically
        let mut transaction = self.create_retryable_trx()?;

        loop {
            // executing the closure
            let result_closure = closure(transaction.clone(), maybe_committed_transaction).await;

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
}

#[cfg(feature = "fdb-7_1")]
/// Holds the information about a tenant
#[derive(Serialize, Deserialize, Debug)]
pub struct TenantInfo {
    pub id: i64,
    pub prefix: Vec<u8>,
    pub name: Vec<u8>,
}

#[cfg(feature = "fdb-7_3")]
/// Holds the information about a tenant
#[derive(Serialize, Deserialize, Debug)]
pub struct TenantInfo {
    pub id: i64,
    pub prefix: FDBTenantPrintableInfo,
    pub name: FDBTenantPrintableInfo,
}

impl TryFrom<(&[u8], &[u8])> for TenantInfo {
    type Error = Error;

    fn try_from(k_v: (&[u8], &[u8])) -> Result<Self, Self::Error> {
        let value = k_v.1;
        match serde_json::from_slice::<FDBTenantInfo>(value) {
            #[cfg(feature = "fdb-7_1")]
            Ok(tenant_info) => Ok(TenantInfo {
                name: k_v.0.split_at(TENANT_MAP_PREFIX.len()).1.to_vec(),
                id: tenant_info.id,
                prefix: tenant_info.prefix,
            }),

            #[cfg(feature = "fdb-7_3")]
            Ok(tenant_info) => Ok(TenantInfo {
                name: tenant_info.name,
                id: tenant_info.id,
                prefix: tenant_info.prefix,
            }),

            Err(err) => Err(err),
        }
    }
}

/// Holds the information about a tenant. This is the struct that is stored in FDB
#[cfg(feature = "fdb-7_1")]
#[derive(Serialize, Deserialize, Debug)]
struct FDBTenantInfo {
    id: i64,
    #[serde(with = "serde_bytes")]
    prefix: Vec<u8>,
}

#[cfg(feature = "fdb-7_3")]
#[derive(Serialize, Deserialize, Debug)]
struct FDBTenantInfo {
    id: i64,
    lock_state: TenantLockState,
    name: FDBTenantPrintableInfo,
    prefix: FDBTenantPrintableInfo,
}

#[cfg(feature = "fdb-7_3")]
#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
enum TenantLockState {
    Unlocked,
    Locked,
    ReadOnly,
}

#[cfg(feature = "fdb-7_3")]
/// Display a printable version of bytes
#[derive(Serialize, Deserialize, Debug)]
pub struct FDBTenantPrintableInfo {
    base64: String,
    printable: String,
}

/// The FoundationDB API includes function to manage the set of tenants in a cluster.
// This is a port from https://github.com/apple/foundationdb/blob/87ee0a2963f615079b3f50afa332acd0ead5f1fe/bindings/java/src/main/com/apple/foundationdb/TenantManagement.java
#[derive(Debug)]
pub struct TenantManagement;

impl TenantManagement {
    /// Creates a new tenant in the cluster using a transaction created on the specified Database.
    /// his operation will first check whether the tenant exists, and if it does it will set the Result
    /// to a `tenant_already_exists` error. Otherwise, it will attempt to create the tenant in a retry loop.
    /// If the tenant is created concurrently by another transaction, this function may still return successfully.
    pub async fn create_tenant(db: &Database, tenant_name: &[u8]) -> Result<(), FdbError> {
        let checked_existence = AtomicBool::new(false);
        let checked_existence_ref = &checked_existence;

        let mut key: Vec<u8> = Vec::with_capacity(TENANT_MAP_PREFIX.len() + tenant_name.len());
        key.extend_from_slice(TENANT_MAP_PREFIX);
        key.extend_from_slice(tenant_name);

        let key_ref = &key;

        db.run(|trx, _maybe_committed| async move {
            trx.set_option(TransactionOption::SpecialKeySpaceEnableWrites)?;

            if checked_existence_ref.load(Ordering::SeqCst) {
                trx.set(key_ref, &[]);
                Ok(())
            } else {
                let maybe_key = trx.get(key_ref, false).await?;

                checked_existence_ref.store(true, Ordering::SeqCst);

                match maybe_key {
                    None => {
                        trx.set(key_ref, &[]);
                        Ok(())
                    }
                    Some(_) => {
                        // `tenant_already_exists` error
                        Err(FdbBindingError::from(FdbError::new(2132)))
                    }
                }
            }
        })
        .await
        // error can only be an fdb_error
        .map_err(|e| e.get_fdb_error().unwrap())
    }

    /// Get a tenant in the cluster using a transaction created on the specified Database.
    pub async fn get_tenant(
        db: &Database,
        tenant_name: &[u8],
    ) -> Result<Option<Result<TenantInfo, serde_json::Error>>, FdbError> {
        let mut key: Vec<u8> = Vec::with_capacity(TENANT_MAP_PREFIX.len() + tenant_name.len());
        key.extend_from_slice(TENANT_MAP_PREFIX);
        key.extend_from_slice(tenant_name);

        let key_ref = &key;
        match db
            .run(|trx, _maybe_committed| async move {
                trx.set_option(TransactionOption::ReadSystemKeys)?;
                trx.set_option(TransactionOption::ReadLockAware)?;

                Ok(trx.get(key_ref, false).await?)
            })
            .await
        {
            Ok(None) => Ok(None),
            Ok(Some(kv)) => Ok(Some(TenantInfo::try_from((key.as_slice(), kv.as_ref())))),
            // error can only be an fdb_error
            Err(err) => Err(err.get_fdb_error().unwrap()),
        }
    }

    /// Deletes a tenant from the cluster using a transaction created on the specified `Database`.
    /// This operation will first check whether the tenant exists, and if it does not it will set the
    /// result to a `tenant_not_found` error. Otherwise, it will attempt to delete the tenant in a retry loop.
    /// If the tenant is deleted concurrently by another transaction, this function may still return successfully.
    pub async fn delete_tenant(db: &Database, tenant_name: &[u8]) -> Result<(), FdbError> {
        let checked_existence = AtomicBool::new(false);
        let checked_existence_ref = &checked_existence;

        let mut key: Vec<u8> = Vec::with_capacity(TENANT_MAP_PREFIX.len() + tenant_name.len());
        key.extend_from_slice(TENANT_MAP_PREFIX);
        key.extend_from_slice(tenant_name);

        let key_ref = &key;

        db.run(|trx, _maybe_committed| async move {
            trx.set_option(TransactionOption::SpecialKeySpaceEnableWrites)?;

            if checked_existence_ref.load(Ordering::SeqCst) {
                trx.clear(key_ref);
                Ok(())
            } else {
                let maybe_key = trx.get(key_ref, false).await?;

                checked_existence_ref.store(true, Ordering::SeqCst);

                match maybe_key {
                    None => {
                        // `tenant_not_found` error
                        Err(FdbBindingError::from(FdbError::new(2131)))
                    }
                    Some(_) => {
                        trx.clear(key_ref);
                        Ok(())
                    }
                }
            }
        })
        .await
        // error can only be an fdb_error
        .map_err(|e| e.get_fdb_error().unwrap())
    }

    /// Lists all tenants in between the range specified. The number of tenants listed can be restricted.
    /// This is a convenience method that generates the begin and end ranges by packing two Tuples.
    pub async fn list_tenant(
        db: &Database,
        begin: &[u8],
        end: &[u8],
        limit: Option<usize>,
    ) -> Result<Vec<Result<TenantInfo, serde_json::Error>>, FdbError> {
        let trx = db.create_trx()?;
        trx.set_option(TransactionOption::ReadSystemKeys)?;
        trx.set_option(TransactionOption::ReadLockAware)?;

        let mut begin_range: Vec<u8> = Vec::with_capacity(TENANT_MAP_PREFIX.len() + begin.len());
        begin_range.extend_from_slice(TENANT_MAP_PREFIX);
        begin_range.extend_from_slice(begin);

        let end_range = if end.is_empty() {
            TENANT_MAP_PREFIX_END.to_vec()
        } else {
            let mut end_range: Vec<u8> = Vec::with_capacity(TENANT_MAP_PREFIX.len() + end.len());
            end_range.extend_from_slice(TENANT_MAP_PREFIX);
            end_range.extend_from_slice(end);

            end_range
        };

        let range_option = RangeOption {
            begin: KeySelector::first_greater_than(begin_range),
            end: KeySelector::first_greater_than(end_range),
            limit,
            ..Default::default()
        };

        trx.get_ranges_keyvalues(range_option, false)
            .map_ok(|fdb_value| TenantInfo::try_from((fdb_value.key(), fdb_value.value())))
            .try_collect::<Vec<Result<TenantInfo, serde_json::Error>>>()
            .await
    }
}
