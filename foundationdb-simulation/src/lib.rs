//! FoundationDB-workload module
//!
//! This module provides all necessary bindings for a FoundationDB's [ExternalWorkload](https://apple.github.io/foundationdb/client-testing.html#simulation-and-cluster-workloads)
//! under a Rust trait, as well as a way to register a Workload in the simulation.

#![warn(missing_docs)]
use std::{mem::ManuallyDrop, os::raw::c_char, ptr::NonNull};

use foundationdb::Database;
use foundationdb_sys::FDBDatabase;

mod fdb_rt;
mod fdb_wrapper;

pub use fdb_rt::fdb_spawn;
use fdb_wrapper::{metrics_extend, opaque, str_for_c, str_from_c};
pub use fdb_wrapper::{CPPWorkloadFactory, Details, Metric, Promise, Severity, WorkloadContext};

// -----------------------------------------------------------------------------
// User friendly types

/// Rust representation of a simulated FoundationDB database
pub type SimDatabase = ManuallyDrop<Database>;
/// Rust representation of a FoundationDB workload
pub type Workload = Box<dyn RustWorkload>;

/// RustWorkload trait provides a one to one equivalent to the C++ abstract class `FDBWorkload`
pub trait RustWorkload {
    /// Return the name or description of the workload.
    /// Primarily used for tracing.
    fn description(&self) -> String;

    /// This method is called by the tester during the setup phase.
    /// It should be used to populate the database.
    ///
    /// # Arguments
    ///
    /// * `db` - The simulated database.
    /// * `done` - A promise that should be resolved to indicate completion
    fn setup(&'static mut self, db: SimDatabase, done: Promise);

    /// This method should run the actual test.
    ///
    /// # Arguments
    ///
    /// * `db` - The simulated database.
    /// * `done` - A promise that should be resolved to indicate completion
    fn start(&'static mut self, db: SimDatabase, done: Promise);

    /// This method is called when the tester completes.
    /// A workload should run any consistency/correctness tests during this phase.
    ///
    /// # Arguments
    ///
    /// * `db` - The simulated database.
    /// * `done` - A promise that should be resolved to indicate completion
    fn check(&'static mut self, db: SimDatabase, done: Promise);

    /// If a workload collects metrics (like latencies or throughput numbers), these should be reported back here.
    /// The multitester (or test orchestrator) will collect all metrics from all test clients and it will aggregate them.
    fn get_metrics(&self) -> Vec<Metric>;

    /// Set the check timeout for this workload.
    fn get_check_timeout(&self) -> f64;
}

// -----------------------------------------------------------------------------
// Hook the user has to define (through `#[simulation_entrypoint])

extern "Rust" {
    fn workload_instantiate_hook(name: &str, context: WorkloadContext) -> Workload;
}

// -----------------------------------------------------------------------------
// C++ to Rust bindings

#[no_mangle]
extern "C" fn workload_instantiate(
    raw_name: *const c_char,
    raw_context: *mut opaque::Context,
) -> *mut Workload {
    let name = str_from_c(raw_name);
    let context = WorkloadContext::new(raw_context);
    let workload = unsafe { workload_instantiate_hook(&name, context) };
    // the `Box<dyn RustWorkload>` is put on the heap with another `Box::new`
    // `Box::into_raw` turns that `Box` into a thin pointer
    // it is this pointer that will be stored in the C++ `WorkloadTranslater`
    // and that is passed to the other `workload_*` functions as `&'static Workload` or `&Workload`
    // `Box::from_raw` will be called by `workload_drop` to clean up everything
    Box::into_raw(Box::new(workload))
}
#[no_mangle]
extern "C" fn workload_description(workload: &Workload) -> *const c_char {
    let description = str_for_c(workload.description());
    // FIXME: the CString will be dropped by Rust before it is read by the C++ side
    // but if Rust doesn't drop it now it's a memory leak...
    // note that that C++ instantly makes a copy so the pointer doesn't stay dangling too long
    description.as_ptr()
}
#[no_mangle]
extern "C" fn workload_setup(
    workload: &'static mut Workload,
    raw_database: NonNull<FDBDatabase>,
    raw_promise: *const opaque::Promise,
) {
    let db = ManuallyDrop::new(Database::new_from_pointer(raw_database));
    let done = Promise::new(raw_promise);
    workload.setup(db, done);
}
#[no_mangle]
extern "C" fn workload_start(
    workload: &'static mut Workload,
    raw_database: NonNull<FDBDatabase>,
    raw_promise: *const opaque::Promise,
) {
    let db = ManuallyDrop::new(Database::new_from_pointer(raw_database));
    let done = Promise::new(raw_promise);
    workload.start(db, done)
}
#[no_mangle]
extern "C" fn workload_check(
    workload: &'static mut Workload,
    raw_database: NonNull<FDBDatabase>,
    raw_promise: *const opaque::Promise,
) {
    let db = ManuallyDrop::new(Database::new_from_pointer(raw_database));
    let done = Promise::new(raw_promise);
    workload.check(db, done)
}
#[no_mangle]
extern "C" fn workload_get_metrics(workload: &Workload, out: *const opaque::Metrics) {
    let metrics = workload.get_metrics();
    metrics_extend(out, metrics)
}
#[no_mangle]
extern "C" fn workload_get_check_timeout(workload: &Workload) -> f64 {
    workload.get_check_timeout()
}
#[no_mangle]
extern "C" fn workload_drop(workload: *mut Workload) {
    unsafe { Box::from_raw(workload) };
}
