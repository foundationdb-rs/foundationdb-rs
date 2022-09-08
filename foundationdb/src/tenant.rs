//! Implementation of the Tenants API

use crate::{error, FdbResult, Transaction};
use foundationdb_sys as fdb_sys;
use std::ptr::NonNull;

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
