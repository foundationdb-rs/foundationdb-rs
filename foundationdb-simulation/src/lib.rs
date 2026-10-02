#![warn(missing_docs)]
#![doc = include_str!("../README.md")]

use std::{
    cell::{Cell, RefCell},
    future::Future,
    mem::ManuallyDrop,
    pin::Pin,
    ptr::NonNull,
    rc::{Rc, Weak},
    sync::Arc,
    task::{Context, Poll},
};

use foundationdb::Database;
use foundationdb_sys::FDBDatabase as FDBDatabaseAlias;

mod bindings;
pub mod env;
mod fdb_rt;

use bindings::{
    FDB_WORKLOAD_API_VERSION, FDBDatabase, FDBMetrics, FDBPromise, FDBWorkload, FDBWorkload_VT,
    OpaqueWorkload, Promise,
};
pub use bindings::{Metric, Metrics, Severity, WorkloadContext};
pub use env::{SimClock, SimRng};
use fdb_rt::fdb_spawn;

// -----------------------------------------------------------------------------
// User friendly types

/// Rust representation of a simulated FoundationDB database
pub type SimDatabase = Arc<Database>;
/// Rust representation of a FoundationDB workload
pub type WrappedWorkload = FDBWorkload;

/// Equivalent to the C++ abstract class `FDBWorkload`
#[allow(async_fn_in_trait)]
pub trait RustWorkload: Sized + 'static {
    /// This method is called by the tester during the setup phase.
    /// It should be used to populate the database.
    ///
    /// # Arguments
    ///
    /// * `db` - The simulated database.
    async fn setup(&mut self, db: SimDatabase);

    /// This method should run the actual test.
    ///
    /// # Arguments
    ///
    /// * `db` - The simulated database.
    async fn start(&mut self, db: SimDatabase);

    /// This method is called when the tester completes.
    /// A workload should run any consistency/correctness tests during this phase.
    ///
    /// # Arguments
    ///
    /// * `db` - The simulated database.
    async fn check(&mut self, db: SimDatabase);

    /// If a workload collects metrics (like latencies or throughput numbers), these should be reported back here.
    /// The multitester (or test orchestrator) will collect all metrics from all test clients and it will aggregate them.
    ///
    /// # Arguments
    ///
    /// * `out` - A metric sink
    fn get_metrics(&self, out: Metrics);

    /// Set the check timeout in simulated seconds for this workload.
    fn get_check_timeout(&self) -> f64;

    /// Virtual Table used by the C API
    const VT: FDBWorkload_VT = FDBWorkload_VT {
        setup: Some(workload_setup::<Self>),
        start: Some(workload_start::<Self>),
        check: Some(workload_check::<Self>),
        getMetrics: Some(workload_get_metrics::<Self>),
        getCheckTimeout: Some(workload_get_check_timeout::<Self>),
        free: Some(workload_drop::<Self>),
    };

    /// Wrap the underlying Rust type so it can be passed to the C API
    fn wrap(self) -> WrappedWorkload {
        let inner = Box::into_raw(Box::new(Rc::new(WorkloadState {
            workload: RefCell::new(Some(Box::new(self))),
            task: RefCell::new(Weak::new()),
            active_check_timeout: Cell::new(None),
            releasing_phase: Cell::new(false),
        })));
        WrappedWorkload {
            api_version: FDB_WORKLOAD_API_VERSION,
            inner: inner as *mut _,
            vt: &Self::VT as *const _ as *mut _,
        }
    }
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

struct WorkloadState<W> {
    workload: RefCell<Option<Box<W>>>,
    task: RefCell<Weak<RefCell<Option<PhaseTask<W>>>>>,
    active_check_timeout: Cell<Option<f64>>,
    releasing_phase: Cell<bool>,
}

// The phase owns W while suspended, rather than borrowing an allocation that the
// native free callback can destroy. Cancellation returns it before owner teardown.
struct PhaseWorkload<W> {
    workload: Option<Box<W>>,
    state: Rc<WorkloadState<W>>,
}

impl<W> Drop for PhaseWorkload<W> {
    fn drop(&mut self) {
        *self.state.workload.borrow_mut() = self.workload.take();
    }
}

// The native caller owns this database reference. The guard always outlives the
// user future, including cancellation, and releases only the Rust Arc allocation.
struct PhaseDatabase(ManuallyDrop<SimDatabase>);

impl Drop for PhaseDatabase {
    fn drop(&mut self) {
        // SAFETY: This is the guard's only release, and ManuallyDrop prevents a
        // borrowed native reference from being destroyed on any error path.
        let database = unsafe { ManuallyDrop::take(&mut self.0) };
        if Arc::strong_count(&database) != 1 || Arc::weak_count(&database) != 0 {
            eprintln!(
                "Reference to Database kept after phase completion or cancellation. All references must be dropped."
            );
            std::process::exit(1);
        }
        let Ok(database) = Arc::try_unwrap(database) else {
            unreachable!("the phase owns the only database reference")
        };
        let _borrowed = ManuallyDrop::new(database);
    }
}

struct PhaseTask<W> {
    future: Option<Pin<Box<dyn Future<Output = ()>>>>,
    database: Option<PhaseDatabase>,
    state: Option<Rc<WorkloadState<W>>>,
    done: Option<Promise>,
}

impl<W> PhaseTask<W> {
    fn release_phase(&mut self) {
        if let Some(state) = self.state.take() {
            state.releasing_phase.set(true);
            drop(self.future.take());
            drop(self.database.take());
            state.releasing_phase.set(false);
            state.active_check_timeout.set(None);
            *state.task.borrow_mut() = Weak::new();
            // Do not retain W across native promise callbacks: resolving or
            // releasing a promise may synchronously free the registered workload.
            drop(state);
        }
    }
}

impl<W> Future for PhaseTask<W> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = self.get_mut();
        if this.future.as_mut().unwrap().as_mut().poll(cx).is_pending() {
            return Poll::Pending;
        }
        this.release_phase();
        this.done.take().unwrap().send(true);
        Poll::Ready(())
    }
}

impl<W> Drop for PhaseTask<W> {
    fn drop(&mut self) {
        self.release_phase();
    }
}

// The runtime owns this driver; the native workload keeps only a weak handle.
// Cancellation empties the slot before any later callback accesses the workload.
// A late wake can then poll an empty driver without touching the old phase.
struct PhaseDriver<W>(Rc<RefCell<Option<PhaseTask<W>>>>);

impl<W> Future for PhaseDriver<W> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let mut slot = self.0.borrow_mut();
        let Some(task) = slot.as_mut() else {
            return Poll::Ready(());
        };
        if Pin::new(task).poll(cx).is_pending() {
            return Poll::Pending;
        }
        slot.take();
        Poll::Ready(())
    }
}

fn cancel_phase<W>(state: &WorkloadState<W>) {
    let handle = state.task.replace(Weak::new());
    if let Some(slot) = handle.upgrade() {
        // Do not hold the slot's borrow across destructors or native callbacks.
        let phase = slot.borrow_mut().take();
        drop(phase);
    }
}

#[cfg(coverage)]
fn write_coverage_profile() {
    unsafe extern "C" {
        fn __llvm_profile_reset_counters();
        fn __llvm_profile_write_file() -> i32;
    }

    // fdbserver exits with `_exit`, so LLVM's normal process-exit hook cannot
    // persist a profile from this dynamically loaded workload.
    let result = unsafe { __llvm_profile_write_file() };
    if result == 0 {
        // Every simulator client shares the DSO's counters. Reset only after a
        // successful snapshot so later client callbacks add, rather than repeat,
        // counts in LLVM_PROFILE_FILE's %m-merged profile.
        unsafe { __llvm_profile_reset_counters() };
    } else {
        eprintln!("failed to write LLVM coverage profile (status {result})");
    }
}

enum Phase {
    Setup,
    Start,
    Check,
}

// A native timeout abandons its waiter without notifying the Rust task.
// Callbacks requiring idle W must end the previous phase's exclusive access.
unsafe fn idle_workload<W: RustWorkload>(
    raw_workload: *mut OpaqueWorkload,
) -> Option<Rc<WorkloadState<W>>> {
    let state = unsafe { &*(raw_workload as *const Rc<WorkloadState<W>>) };
    let weak = Rc::downgrade(state);
    // Releasing the old promise can synchronously free the native owner.
    // A strong Rc here would delay W's destructor past context invalidation.
    cancel_phase(state);
    weak.upgrade()
}

unsafe fn spawn_phase<W: RustWorkload>(
    raw_workload: *mut OpaqueWorkload,
    raw_database: *mut FDBDatabase,
    raw_promise: FDBPromise,
    phase: Phase,
) {
    // SAFETY: wrap allocates this owner and the ABI invokes its callbacks on the
    // same thread, serially, until its unique free callback.
    let Some(state) = (unsafe { idle_workload::<W>(raw_workload) }) else {
        drop(Promise::new(raw_promise));
        return;
    };
    let workload = state
        .workload
        .borrow_mut()
        .take()
        .expect("workload phases cannot overlap");
    // Native callers may evaluate check() before getCheckTimeout(). Sampling
    // while W is idle avoids aliasing a suspended phase's exclusive borrow.
    if matches!(phase, Phase::Check) {
        state
            .active_check_timeout
            .set(Some(workload.get_check_timeout()));
    }
    let mut workload = PhaseWorkload {
        workload: Some(workload),
        state: state.clone(),
    };
    let ptr = NonNull::new(raw_database as *mut FDBDatabaseAlias)
        .expect("the simulator must supply a database");
    // SAFETY: The native caller supplies a live pointer for this phase. The
    // guard suppresses Database::drop and rejects references escaping the phase.
    let database = PhaseDatabase(ManuallyDrop::new(Arc::new(unsafe {
        Database::new_from_pointer(ptr)
    })));
    let borrowed_database = Arc::clone(&database.0);
    let future = async move {
        let inner = workload.workload.as_mut().unwrap();
        match phase {
            Phase::Setup => inner.setup(borrowed_database).await,
            Phase::Start => inner.start(borrowed_database).await,
            Phase::Check => inner.check(borrowed_database).await,
        }
        // Keep the owning guard, rather than only its workload field, captured
        // until the user future has released every borrow of W.
        drop(workload);
    };
    let task = PhaseTask {
        future: Some(Box::pin(future)),
        database: Some(database),
        state: Some(state.clone()),
        done: Some(Promise::new(raw_promise)),
    };
    let slot = Rc::new(RefCell::new(Some(task)));
    *state.task.borrow_mut() = Rc::downgrade(&slot);
    drop(state);
    fdb_spawn(PhaseDriver(slot));
}

unsafe extern "C" fn workload_setup<W: RustWorkload>(
    raw_workload: *mut OpaqueWorkload,
    raw_database: *mut FDBDatabase,
    raw_promise: FDBPromise,
) {
    unsafe { spawn_phase::<W>(raw_workload, raw_database, raw_promise, Phase::Setup) };
}

unsafe extern "C" fn workload_start<W: RustWorkload>(
    raw_workload: *mut OpaqueWorkload,
    raw_database: *mut FDBDatabase,
    raw_promise: FDBPromise,
) {
    unsafe { spawn_phase::<W>(raw_workload, raw_database, raw_promise, Phase::Start) };
}

unsafe extern "C" fn workload_check<W: RustWorkload>(
    raw_workload: *mut OpaqueWorkload,
    raw_database: *mut FDBDatabase,
    raw_promise: FDBPromise,
) {
    unsafe { spawn_phase::<W>(raw_workload, raw_database, raw_promise, Phase::Check) };
}
unsafe extern "C" fn workload_get_metrics<W: RustWorkload>(
    raw_workload: *mut OpaqueWorkload,
    raw_metrics: FDBMetrics,
) {
    unsafe {
        let Some(state) = idle_workload::<W>(raw_workload) else {
            return;
        };
        let workload = state.workload.borrow();
        let workload = workload
            .as_ref()
            .expect("cancellation restores the workload");
        let out = Metrics::new(raw_metrics);
        workload.get_metrics(out);
        #[cfg(coverage)]
        write_coverage_profile();
    }
}
unsafe extern "C" fn workload_get_check_timeout<W: RustWorkload>(
    raw_workload: *mut OpaqueWorkload,
) -> f64 {
    unsafe {
        let state = &*(raw_workload as *const Rc<WorkloadState<W>>);
        if let Some(timeout) = state.active_check_timeout.get() {
            // Check can still hold an exclusive borrow of W when the native
            // caller evaluates the timeout after starting that phase.
            return timeout;
        }
        // The opposite argument order can leave an abandoned setup/start
        // holding W. Cancel it before reading the workload's current timeout.
        let Some(state) = idle_workload::<W>(raw_workload) else {
            // Releasing the old promise synchronously freed the native owner.
            return 0.0;
        };
        let workload = state.workload.borrow();
        workload
            .as_ref()
            .expect("cancellation restores the workload")
            .get_check_timeout()
    }
}
unsafe extern "C" fn workload_drop<W: RustWorkload>(raw_workload: *mut OpaqueWorkload) {
    let owner = unsafe { &*(raw_workload as *const Rc<WorkloadState<W>>) };
    assert!(
        !owner.releasing_phase.get(),
        "the native caller cannot free a workload during its phase destructor"
    );
    let state = unsafe { Box::from_raw(raw_workload as *mut Rc<WorkloadState<W>>) };
    cancel_phase(&state);
    drop(state);
}

// -----------------------------------------------------------------------------
// Registration hooks

#[doc(hidden)]
/// Primitives exposed for the registrations hooks, should not be used otherwise
pub mod internals {
    pub use crate::bindings::{FDBWorkloadContext, str_from_c};
    pub use crate::fdb_rt::poll_pending_tasks;

    #[cfg(feature = "cpp-abi")]
    unsafe extern "C" {
        pub fn workloadCppFactory(logger: *const u8) -> *const u8;
    }

    #[allow(non_snake_case)]
    #[cfg(not(feature = "cpp-abi"))]
    pub unsafe extern "C" fn workloadCppFactory(_logger: *const u8) -> *const u8 {
        eprintln!(
            "This Rust workload was compiled without the C++ shim adapter. To fix this, either:

- Re-run the simulation with `useCAPI = true` (FoundationDB 7.4 or newer), or
- Recompile the workload with FoundationDB versions prior to 7.4 or the `cpp-abi` feature"
        );
        std::process::exit(1);
    }
}

/// Register a [RustWorkloadFactory].
/// /!\ Should be called only once.
#[macro_export]
macro_rules! register_factory {
    ($name:ident) => {
        #[unsafe(no_mangle)]
        extern "C" fn workloadCFactory(
            raw_name: *const std::ffi::c_char,
            raw_context: $crate::internals::FDBWorkloadContext,
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
                foundationdb::future::CUSTOM_EXECUTOR_HOOK
                    .set($crate::internals::poll_pending_tasks)
                    .unwrap();
            }
            let name = $crate::internals::str_from_c(raw_name);
            let context = $crate::WorkloadContext::new(raw_context);
            <$name as $crate::RustWorkloadFactory>::create(name, context)
        }
        #[unsafe(no_mangle)]
        extern "C" fn workloadFactory(logger: *const u8) -> *const u8 {
            unsafe { $crate::internals::workloadCppFactory(logger) }
        }
    };
}

/// Register a [SingleRustWorkload] and creates an implicit WorkloadFactory.
/// /!\ Should be called only once.
#[macro_export]
macro_rules! register_workload {
    ($name:ident) => {
        #[unsafe(no_mangle)]
        extern "C" fn workloadCFactory(
            raw_name: *const std::ffi::c_char,
            raw_context: $crate::internals::FDBWorkloadContext,
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
                foundationdb::future::CUSTOM_EXECUTOR_HOOK
                    .set($crate::internals::poll_pending_tasks)
                    .unwrap();
            }
            let name = $crate::internals::str_from_c(raw_name);
            let context = $crate::WorkloadContext::new(raw_context);
            $crate::RustWorkload::wrap(<$name as $crate::SingleRustWorkload>::new(name, context))
        }
        #[unsafe(no_mangle)]
        extern "C" fn workloadFactory(logger: *const u8) -> *const u8 {
            unsafe { $crate::internals::workloadCppFactory(logger) }
        }
    };
}
