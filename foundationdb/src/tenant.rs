//! Implementation of the Tenants API

use crate::options::TransactionOption;

use crate::{
    error, Database, FdbBindingError, FdbError, FdbResult, KeySelector, RangeOption, Transaction,
};
use foundationdb_sys as fdb_sys;

use futures::TryStreamExt;
use serde::{Deserialize, Serialize};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};

const TENANT_MAP_PREFIX: &[u8] = b"\xFF\xFF/management/tenant_map/";

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
    /// Returns the name of this [`Tenant`].
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
}

/// Holds the information about a tenant
#[derive(Serialize, Deserialize)]
pub struct TenantInfo {
    pub id: Vec<u8>,
    pub prefix: Vec<u8>,
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
    ) -> Result<Vec<Option<TenantInfo>>, FdbError> {
        let trx = db.create_trx()?;
        trx.set_option(TransactionOption::ReadSystemKeys)?;
        trx.set_option(TransactionOption::ReadLockAware)?;

        let mut begin_range: Vec<u8> = Vec::with_capacity(TENANT_MAP_PREFIX.len() + begin.len());
        begin_range.extend_from_slice(TENANT_MAP_PREFIX);
        begin_range.extend_from_slice(begin);

        let mut end_range: Vec<u8> = Vec::with_capacity(TENANT_MAP_PREFIX.len() + end.len());
        end_range.extend_from_slice(TENANT_MAP_PREFIX);
        end_range.extend_from_slice(end);

        let range_option = RangeOption {
            begin: KeySelector::first_greater_than(begin_range),
            end: KeySelector::last_less_than(end_range),
            limit,
            ..Default::default()
        };

        trx.get_ranges_keyvalues(range_option, false)
            .map_ok(
                |fdb_value| match serde_json::from_slice::<TenantInfo>(fdb_value.value()) {
                    Ok(tenant_info) => Some(tenant_info),
                    Err(_) => None,
                },
            )
            .try_collect::<Vec<Option<TenantInfo>>>()
            .await
    }
}
