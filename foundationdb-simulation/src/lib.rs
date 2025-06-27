//! FoundationDB-workload module
//!
//! This module provides all necessary bindings for a FoundationDB's [ExternalWorkload](https://apple.github.io/foundationdb/client-testing.html#simulation-and-cluster-workloads)
//! under a Rust trait, as well as a way to register a Workload in the simulation.

#![warn(missing_docs)]
#![doc = include_str!("../README.md")]

use std::{mem::ManuallyDrop, ptr::NonNull};

use foundationdb::Database as DatabaseAlias;
use foundationdb_sys::FDBDatabase as FDBDatabaseAlias;

mod bindings;
mod fdb_rt;

pub use bindings::{
    str_from_c, FDBWorkloadContext, Metric, Metrics, Promise, Severity, WorkloadContext,
};
use bindings::{FDBDatabase, FDBMetrics, FDBPromise, FDBWorkload, OpaqueWorkload};
pub use fdb_rt::*;

// -----------------------------------------------------------------------------
// User friendly types

/// Rust representation of a simulated FoundationDB database
pub type Database = ManuallyDrop<DatabaseAlias>;
/// Rust representation of a FoundationDB workload
pub type WrappedWorkload = FDBWorkload;

/// Equivalent to the C++ abstract class `FDBWorkload`
pub trait RustWorkload {
    /// This method is called by the tester during the setup phase.
    /// It should be used to populate the database.
    ///
    /// # Arguments
    ///
    /// * `db` - The simulated database.
    /// * `done` - A promise that should be resolved to indicate completion
    fn setup(&'static mut self, db: Database, done: Promise);

    /// This method should run the actual test.
    ///
    /// # Arguments
    ///
    /// * `db` - The simulated database.
    /// * `done` - A promise that should be resolved to indicate completion
    fn start(&'static mut self, db: Database, done: Promise);

    /// This method is called when the tester completes.
    /// A workload should run any consistency/correctness tests during this phase.
    ///
    /// # Arguments
    ///
    /// * `db` - The simulated database.
    /// * `done` - A promise that should be resolved to indicate completion
    fn check(&'static mut self, db: Database, done: Promise);

    /// If a workload collects metrics (like latencies or throughput numbers), these should be reported back here.
    /// The multitester (or test orchestrator) will collect all metrics from all test clients and it will aggregate them.
    ///
    /// # Arguments
    ///
    /// * `out` - A metric sink
    fn get_metrics(&self, out: Metrics);

    /// Set the check timeout in simulated seconds for this workload.
    fn get_check_timeout(&self) -> f64;
}

/// Equivalent to the C++ abstract class `FDBWorkloadFactory`
pub trait RustWorkloadFactory {
    /// The runtime FDB_API_VERSION to use
    const FDB_API_VERSION: u32 = foundationdb_sys::FDB_API_VERSION;
    /// If the test file contains a key-value pair workloadName the value will be passed to this method (empty string otherwise).
    /// This way, a library author can implement many workloads in one library and use the test file to chose which one to run
    /// (or run multiple workloads either concurrently or serially).
    fn create(name: String, context: WorkloadContext) -> WrappedWorkload;
}

/// Automatically implements a WorkloadFactory for a single workload
pub trait SingleRustWorkload: RustWorkload {
    /// The runtime FDB_API_VERSION to use
    const FDB_API_VERSION: u32 = foundationdb_sys::FDB_API_VERSION;
    /// The implicit WorkloadFactory will call this method uppon each instantiation
    fn new(name: String, context: WorkloadContext) -> Self;
}

// -----------------------------------------------------------------------------
// C to Rust bindings

unsafe fn database_new(raw_database: *mut FDBDatabase) -> Database {
    ManuallyDrop::new(DatabaseAlias::new_from_pointer(NonNull::new_unchecked(
        raw_database as *mut FDBDatabaseAlias,
    )))
}
unsafe extern "C" fn workload_setup<W: RustWorkload + 'static>(
    raw_workload: *mut OpaqueWorkload,
    raw_database: *mut FDBDatabase,
    raw_promise: FDBPromise,
) {
    let workload = &mut *(raw_workload as *mut W);
    let database = database_new(raw_database);
    let done = Promise::new(raw_promise);
    workload.setup(database, done)
}
unsafe extern "C" fn workload_start<W: RustWorkload + 'static>(
    raw_workload: *mut OpaqueWorkload,
    raw_database: *mut FDBDatabase,
    raw_promise: FDBPromise,
) {
    let workload = &mut *(raw_workload as *mut W);
    let database = database_new(raw_database);
    let done = Promise::new(raw_promise);
    workload.start(database, done)
}
unsafe extern "C" fn workload_check<W: RustWorkload + 'static>(
    raw_workload: *mut OpaqueWorkload,
    raw_database: *mut FDBDatabase,
    raw_promise: FDBPromise,
) {
    let workload = &mut *(raw_workload as *mut W);
    let database = database_new(raw_database);
    let done = Promise::new(raw_promise);
    workload.check(database, done)
}
unsafe extern "C" fn workload_get_metrics<W: RustWorkload>(
    raw_workload: *mut OpaqueWorkload,
    raw_metrics: FDBMetrics,
) {
    let workload = &*(raw_workload as *mut W);
    let out = Metrics::new(raw_metrics);
    workload.get_metrics(out)
}
unsafe extern "C" fn workload_get_check_timeout<W: RustWorkload>(
    raw_workload: *mut OpaqueWorkload,
) -> f64 {
    let workload = &*(raw_workload as *mut W);
    workload.get_check_timeout()
}
unsafe extern "C" fn workload_drop<W: RustWorkload>(raw_workload: *mut OpaqueWorkload) {
    unsafe { drop(Box::from_raw(raw_workload as *mut W)) };
}

impl WrappedWorkload {
    pub fn new<W: RustWorkload + 'static>(workload: W) -> Self {
        let workload = Box::into_raw(Box::new(workload));
        WrappedWorkload {
            inner: workload as *mut _,
            setup: Some(workload_setup::<W>),
            start: Some(workload_start::<W>),
            check: Some(workload_check::<W>),
            getMetrics: Some(workload_get_metrics::<W>),
            getCheckTimeout: Some(workload_get_check_timeout::<W>),
            free: Some(workload_drop::<W>),
        }
    }
}
// -----------------------------------------------------------------------------
// Registration hooks

/// Register a [RustWorkloadFactory].
/// /!\ Should be called only once.
#[macro_export]
macro_rules! register_factory {
    ($name:ident) => {
        #[no_mangle]
        extern "C" fn workloadCFactory(
            raw_name: *const i8,
            raw_context: $crate::FDBWorkloadContext,
        ) -> $crate::WrappedWorkload {
            use std::sync::atomic::{AtomicBool, Ordering};
            static DONE: AtomicBool = AtomicBool::new(false);
            if DONE
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                let version = <$name as $crate::RustWorkloadFactory>::FDB_API_VERSION;
                let _ = foundationdb::api::FdbApiBuilder::default()
                    .set_runtime_version(version as i32)
                    .build();
                println!("FDB API version selected: {version}");
            }
            let name = $crate::str_from_c(raw_name);
            let context = $crate::WorkloadContext::new(raw_context);
            <$name as $crate::RustWorkloadFactory>::create(name, context)
        }
        extern "C" {
            pub fn workloadCppFactory(logger: *const u8) -> *const u8;
        }
        #[cfg(feature = "cpp-abi")]
        #[no_mangle]
        extern "C" fn workloadFactory(logger: *const u8) -> *const u8 {
            unsafe { workloadCppFactory(logger) }
        }
    };
}

/// Register a [SingleRustWorkload] and creates an implicit WorkloadFactory.
/// /!\ Should be called only once.
#[macro_export]
macro_rules! register_workload {
    ($name:ident) => {
        #[no_mangle]
        extern "C" fn workloadCFactory(
            raw_name: *const i8,
            raw_context: $crate::FDBWorkloadContext,
        ) -> $crate::WrappedWorkload {
            use std::sync::atomic::{AtomicBool, Ordering};
            static DONE: AtomicBool = AtomicBool::new(false);
            if DONE
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                let version = <$name as $crate::SingleRustWorkload>::FDB_API_VERSION;
                let _ = foundationdb::api::FdbApiBuilder::default()
                    .set_runtime_version(version as i32)
                    .build();
                println!("FDB API version selected: {version}");
            }
            let name = $crate::str_from_c(raw_name);
            let context = $crate::WorkloadContext::new(raw_context);
            $crate::WrappedWorkload::new(<$name as $crate::SingleRustWorkload>::new(name, context))
        }
        extern "C" {
            pub fn workloadCppFactory(logger: *const u8) -> *const u8;
        }
        #[cfg(feature = "cpp-abi")]
        #[no_mangle]
        extern "C" fn workloadFactory(logger: *const u8) -> *const u8 {
            unsafe { workloadCppFactory(logger) }
        }
    };
}
