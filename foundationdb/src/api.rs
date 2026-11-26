// Copyright 2018 foundationdb-rs developers, https://github.com/Clikengo/foundationdb-rs/graphs/contributors
// Copyright 2013-2018 Apple, Inc and the FoundationDB project authors.
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Configuration of foundationDB API and Network
//!
//! Provides a safe wrapper around the following C API calls:
//!
//! - [API versioning](https://apple.github.io/foundationdb/api-c.html#api-versioning)
//! - [Network](https://apple.github.io/foundationdb/api-c.html#network)
//!
//! The network lifecycle is tracked by a process-global state machine
//! (`Uninitialized -> ApiVersionSelected -> Running -> Stopped`). Booting is safe
//! and idempotent, the network is started lazily by [`crate::Database`]
//! constructors if needed, and it runs until process exit, where it is stopped and
//! the network thread joined, as required by the C API (see "Stopping the network
//! at process exit" in the source of this module for how and why). Applications
//! that manage their own shutdown can call [`disable_stop_on_exit`] and/or
//! [`stop_network`].
//!
//! <div class="warning">
//!
//! The automatic stop at process exit is meant for tests and short-lived tools.
//! In a production application, prefer a clean teardown: the network thread is
//! the event loop driving every transaction and you may still have on-going
//! operations at exit time. Finish or cancel your work, drop the
//! [`crate::Database`] handles, then call [`stop_network`] yourself.
//!
//! </div>

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

use crate::options::NetworkOption;
use crate::{FdbError, FdbResult, error};
use foundationdb_sys as fdb_sys;

/// Returns the max api version of the underlying Fdb C API Client
pub fn get_max_api_version() -> i32 {
    unsafe { fdb_sys::fdb_get_max_api_version() }
}

// FDB C error codes used by the lifecycle state machine
const ERR_API_VERSION_UNSET: i32 = 2200;
const ERR_API_VERSION_ALREADY_SET: i32 = 2201;
const ERR_API_VERSION_NOT_SUPPORTED: i32 = 2203;
const ERR_NETWORK_ALREADY_SETUP: i32 = 2009;
const ERR_NETWORK_CANNOT_BE_RESTARTED: i32 = 2025;

/// Process-global lifecycle of the FoundationDB client.
///
/// The C API allows selecting the API version and setting up the network once per
/// process, and the network can never be restarted once stopped. Every FFI call
/// involved lives in exactly one transition below, serialized by the `NETWORK`
/// mutex.
enum NetworkLifecycle {
    Uninitialized,
    ApiVersionSelected {
        api_version: i32,
    },
    Running {
        api_version: i32,
        handle: JoinHandle<()>,
    },
    Stopped {
        api_version: i32,
    },
}

static NETWORK: Mutex<NetworkLifecycle> = Mutex::new(NetworkLifecycle::Uninitialized);
static STOP_ON_EXIT: AtomicBool = AtomicBool::new(true);
// Set once this process started the network (fdb_setup_network succeeded), so
// that the exit hook is only ever re-registered by a process that registered it
// in the first place (never by a simulation workload, which uses a network it
// did not start).
static NETWORK_STARTED: AtomicBool = AtomicBool::new(false);
// Set once the exit hook was re-registered after the first completed future.
static EXIT_HOOK_REARMED: AtomicBool = AtomicBool::new(false);
// Last error returned by fdb_run_network (0 = none). An atomic, not a mutex: the
// network thread must never take a lock shared with the thread joining it.
static NETWORK_RUN_ERROR: AtomicI32 = AtomicI32::new(0);

fn lock_network() -> MutexGuard<'static, NetworkLifecycle> {
    // State is only mutated after the corresponding FFI call succeeded, so a
    // poisoned lock still guards a consistent state. The one exception is a
    // thread-spawn panic in start_network after fdb_setup_network succeeded
    // (OS resource exhaustion): the state then stays ApiVersionSelected while
    // the network is set up, and any retry fails with 2009. Deliberately not
    // handled: nothing sensible can be done at that point.
    NETWORK.lock().unwrap_or_else(PoisonError::into_inner)
}

impl NetworkLifecycle {
    fn select_api_version(&mut self, version: i32) -> FdbResult<()> {
        match self {
            NetworkLifecycle::Uninitialized => {
                error::eval(unsafe {
                    fdb_sys::fdb_select_api_version_impl(version, fdb_sys::FDB_API_VERSION as i32)
                })
                .inspect_err(|e| {
                    // generally means the local libfdb doesn't support the requested version
                    if e.code() == ERR_API_VERSION_NOT_SUPPORTED {
                        let max_api_version = get_max_api_version();
                        if max_api_version < fdb_sys::FDB_API_VERSION as i32 {
                            eprintln!(
                                "The version of FoundationDB binding requested '{}' is not supported",
                                fdb_sys::FDB_API_VERSION
                            );
                            eprintln!(
                                "by the installed FoundationDB C library. Maximum supported version by the local library is {max_api_version}"
                            );
                        }
                    }
                })?;
                *self = NetworkLifecycle::ApiVersionSelected {
                    api_version: version,
                };
                Ok(())
            }
            NetworkLifecycle::ApiVersionSelected { api_version }
            | NetworkLifecycle::Running { api_version, .. }
            | NetworkLifecycle::Stopped { api_version } => {
                if *api_version == version {
                    Ok(())
                } else {
                    Err(FdbError::from_code(ERR_API_VERSION_ALREADY_SET))
                }
            }
        }
    }

    fn set_network_option(&mut self, option: NetworkOption) -> FdbResult<()> {
        match self {
            // unreachable through the public API: a NetworkBuilder only exists once
            // an api version is selected
            NetworkLifecycle::Uninitialized => Err(FdbError::from_code(ERR_API_VERSION_UNSET)),
            NetworkLifecycle::ApiVersionSelected { .. } => {
                // SAFETY: the api version is selected and the network is not set up
                // yet; calls are serialized by the NETWORK mutex.
                unsafe { option.apply() }
            }
            NetworkLifecycle::Running { .. } => Err(FdbError::from_code(ERR_NETWORK_ALREADY_SETUP)),
            NetworkLifecycle::Stopped { .. } => {
                Err(FdbError::from_code(ERR_NETWORK_CANNOT_BE_RESTARTED))
            }
        }
    }

    fn start_network(&mut self) -> FdbResult<()> {
        if matches!(self, NetworkLifecycle::Uninitialized) {
            self.select_api_version(fdb_sys::FDB_API_VERSION as i32)?;
        }
        match self {
            NetworkLifecycle::Uninitialized => unreachable!("the api version was just selected"),
            NetworkLifecycle::ApiVersionSelected { api_version } => {
                let api_version = *api_version;
                error::eval(unsafe { fdb_sys::fdb_setup_network() })?;
                // Layer 3 of the exit-time stop, see "Stopping the network at
                // process exit" below. Layers 1 and 2 are armed elsewhere.
                register_exit_hook();
                NETWORK_STARTED.store(true, Ordering::Release);
                let handle = std::thread::Builder::new()
                    .name("fdb-network".to_string())
                    .spawn(run_network_thread)
                    .expect("failed to spawn the fdb-network thread");
                *self = NetworkLifecycle::Running {
                    api_version,
                    handle,
                };
                Ok(())
            }
            NetworkLifecycle::Running { .. } => Ok(()),
            NetworkLifecycle::Stopped { .. } => {
                Err(FdbError::from_code(ERR_NETWORK_CANNOT_BE_RESTARTED))
            }
        }
    }

    fn stop_network(&mut self) -> FdbResult<()> {
        match self {
            NetworkLifecycle::Uninitialized => Ok(()),
            NetworkLifecycle::ApiVersionSelected { api_version } => {
                // Nothing is running, but an explicit stop must be terminal so that
                // a later lazy boot cannot restart the network behind the caller's
                // back.
                *self = NetworkLifecycle::Stopped {
                    api_version: *api_version,
                };
                Ok(())
            }
            NetworkLifecycle::Running { .. } => {
                // Stop before taking the variant apart: on error the state stays
                // Running, handle included.
                error::eval(unsafe { fdb_sys::fdb_stop_network() })?;
                let NetworkLifecycle::Running {
                    api_version,
                    handle,
                } = std::mem::replace(self, NetworkLifecycle::Uninitialized)
                else {
                    unreachable!("state was just matched as Running");
                };
                if handle.join().is_err() {
                    // run_network_thread cannot panic; keep the exit path panic-free
                    // anyway (eprintln! panics if the stderr write fails).
                    let _ = writeln!(std::io::stderr(), "the fdb-network thread panicked");
                }
                *self = NetworkLifecycle::Stopped { api_version };
                Ok(())
            }
            NetworkLifecycle::Stopped { .. } => Ok(()),
        }
    }
}

/// Body of the "fdb-network" thread.
///
/// Invariant: this function must never lock `NETWORK`, as `stop_network` joins the
/// thread while holding the lock.
fn run_network_thread() {
    if let Err(err) = error::eval(unsafe { fdb_sys::fdb_run_network() }) {
        NETWORK_RUN_ERROR.store(err.code(), Ordering::Release);
        let _ = writeln!(
            std::io::stderr(),
            "fdb_run_network returned an error: {err}"
        );
    }
}

// # Stopping the network at process exit
//
// The C API requires `fdb_stop_network()` and a join of the thread running
// `fdb_run_network()` before the process exits. Since the network belongs to the
// process (#195), we do it automatically at exit. *When* exactly we do it matters,
// because of how libfdb_c is written.
//
// ## The problem (foundationdb-rs#585)
//
// libfdb_c is C++. Its function-local statics are constructed lazily, by whichever
// thread first reaches them, and their destructors are registered with
// `__cxa_atexit` at that moment. `exit()` runs everything registered with
// `atexit`/`__cxa_atexit` in a single list, in reverse order of registration.
//
// Some of those statics are first reached by the network thread, after the
// network started, and are used again by the network thread while it shuts down.
// With client tracing enabled, the culprit is `static StatisticsState statState`
// in `systemMonitor()` (flow/SystemMonitor.cpp): it is first built when a
// database connects (`initializeClientTracing`, fdbclient/NativeAPI.cpp), and
// `runNetwork()` calls `systemMonitor()` again right after the event loop returns.
// gdb shows 3 such late registrations from the network thread, all before the
// first transaction completes, and one more during `fdb_stop_network()` itself.
//
// An `atexit` hook registered when the network starts is older than these
// destructors, so it runs after them: the network thread then shuts down on
// destroyed state and the process dies with SIGSEGV in `fdb_run_network`, while
// the trace file is left truncated (no stop events, no closing tag).
// Registrations made *during* our stop are harmless: they happen while our hook
// runs, so their destructors run after it, once the network thread is joined.
//
// ## What the other bindings do
//
// - Python stops and joins from a Python `atexit` handler, and Java from a JVM
//   shutdown hook. Both run during runtime shutdown, before libc `exit()` starts,
//   so before any libfdb_c destructor.
// - Go never stops the network at exit; a Go process exits without running C++
//   static destructors at all.
// - C and Flow programs (fdbcli, fdbserver) run the network on their own main
//   thread, so `fdb_run_network()` returned before `exit()` begins.
//
// Rust has no runtime shutdown hook, and a library cannot make the application
// skip the libc exit sequence. Fixing libfdb_c (never destroying these statics,
// as `~TraceLog` already does for its writer thread) is out of scope here.
//
// ## Our approach: three layers, the first to run wins
//
// Every layer goes through `stop_network_on_exit`, which stops and joins the
// network if it is still running: the first layer to run does the work and the
// others find the network `Stopped` and do nothing.
//
// 1. A thread-local guard on the main thread (`MainThreadExitGuard`, Linux and
//    Apple platforms). `exit()` runs the thread-local destructors of the thread
//    calling it *before* the atexit list (glibc `__run_exit_handlers` calls
//    `__call_tls_dtors` first; Apple's `exit()` runs `_tlv_atexit` destructors
//    first), and Rust std registers its thread-local destructors through these
//    mechanisms. So this layer runs before every libfdb_c destructor, including
//    ones a future libfdb_c might create at any time. The guard is armed from a
//    load-time constructor (`#[ctor]`, which runs on the main thread before
//    `main`), so it does not matter which thread starts the network: plain `main`,
//    `#[tokio::main]`, `tokio::spawn`, libtest worker threads are all covered.
//    It only fires when the main thread is the one exiting (returning from `main`,
//    `std::process::exit` on the main thread). It is best-effort: Rust does not
//    guarantee that main-thread destructors run at exit, and they do not on musl
//    or with a statically linked glibc, where layers 2 and 3 take over.
//
// 2. The atexit hook, registered again after the first FDB future completes
//    (`rearm_exit_hook_after_first_completed_future`, called from
//    `FdbFuture::poll`). A future can only complete once its database is set up
//    on the network thread, which is where libfdb_c creates the late statics
//    above, so this registration is younger than their destructors and runs
//    before them. Portable (musl included) and independent of the thread that
//    exits, it covers what layer 1 cannot. Its limit: it only beats statics that
//    exist by the first completed future. A static created later and used on the
//    shutdown path would break it again, which tests/exit_with_client_trace.rs
//    exists to catch on each libfdb_c upgrade.
//
// 3. The atexit hook registered when the network starts. Last resort, e.g. for a
//    process that creates a `Database` and exits without completing any future on
//    a platform where layer 1 does not run.
//
// None of this replaces an explicit `stop_network()` in a production shutdown
// sequence, which avoids exit-handler ordering altogether.

/// Registers [`network_atexit_hook`] with libc `atexit` (layers 2 and 3).
fn register_exit_hook() {
    if unsafe { libc::atexit(network_atexit_hook) } != 0 {
        let _ = writeln!(
            std::io::stderr(),
            "failed to register the FoundationDB atexit hook; the network may not be stopped automatically at process exit"
        );
    }
}

/// Layer 2: re-registers the exit hook once, after the first completed future.
///
/// Called on every `FdbFuture` completion, so the fast path is a single atomic
/// load.
pub(crate) fn rearm_exit_hook_after_first_completed_future() {
    if EXIT_HOOK_REARMED.load(Ordering::Relaxed) || !NETWORK_STARTED.load(Ordering::Acquire) {
        return;
    }
    if !EXIT_HOOK_REARMED.swap(true, Ordering::AcqRel) {
        register_exit_hook();
    }
}

/// Layer 1: stops the network when the main thread runs its thread-local
/// destructors at exit, before the atexit list.
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
struct MainThreadExitGuard;

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
impl Drop for MainThreadExitGuard {
    fn drop(&mut self) {
        stop_network_on_exit();
    }
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
thread_local! {
    static MAIN_THREAD_EXIT_GUARD: MainThreadExitGuard = const { MainThreadExitGuard };
}

/// Arms layer 1 at load time.
///
/// In an executable, constructors run on the main thread before `main`. The main
/// thread check only matters when this crate is part of a library loaded with
/// `dlopen` from another thread: arming that thread would stop the network
/// whenever it ends. Touching a const-initialized thread-local only registers its
/// destructor, which is a no-op unless the network is running at exit, so this is
/// free for processes that never use FoundationDB (or only a simulated network).
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
#[ctor::ctor(unsafe)]
fn arm_main_thread_exit_guard() {
    #[cfg(target_os = "linux")]
    // SAFETY: both calls are always safe.
    let is_main_thread = unsafe { libc::gettid() == libc::getpid() };
    #[cfg(target_vendor = "apple")]
    // SAFETY: always safe.
    let is_main_thread = unsafe { libc::pthread_main_np() != 0 };
    if is_main_thread {
        MAIN_THREAD_EXIT_GUARD.with(|_| {});
    }
}

/// The atexit hook of layers 2 and 3.
///
/// Panic-free: unwinding out of an `extern "C"` function would abort with a less
/// useful message.
extern "C" fn network_atexit_hook() {
    stop_network_on_exit();
}

/// Stops and joins the network at process exit if it is still running. Shared by
/// every layer above; panic-free, as it runs from an `extern "C"` atexit hook and
/// from a thread-local destructor.
fn stop_network_on_exit() {
    if !STOP_ON_EXIT.load(Ordering::Acquire) {
        return;
    }
    if let Err(err) = lock_network().stop_network() {
        // Exiting with the network still running is undefined behavior, aborting
        // is not.
        let _ = writeln!(
            std::io::stderr(),
            "failed to stop the FoundationDB network at exit: {err}"
        );
        std::process::abort();
    }
}

/// Stops the FoundationDB network and joins the network thread.
///
/// This is terminal for the process: once it returns `Ok`, every FoundationDB
/// operation fails with error 2025 (`network_cannot_be_restarted`), including in
/// other threads. It is idempotent. One exception to terminality: if the client
/// was never initialized at all (no boot, no API version selected, no `Database`
/// created), the call is a plain no-op and a later boot still succeeds.
///
/// Tests and short-lived tools can simply rely on the automatic stop at process
/// exit. In a production application, prefer calling this yourself as part of a
/// clean shutdown sequence, once on-going operations are finished and the
/// [`crate::Database`] handles are dropped.
///
/// Must not be called from the network thread itself (e.g. from a future
/// callback), as it joins that thread.
///
/// # Examples
///
/// ```rust
/// foundationdb::boot().expect("failed to initialize FoundationDB");
/// // ... use FoundationDB ...
/// foundationdb::api::stop_network().expect("failed to stop the network");
/// // The stop is terminal: any use of FoundationDB now fails with error 2025.
/// assert_eq!(foundationdb::boot().err().map(|e| e.code()), Some(2025));
/// ```
pub fn stop_network() -> FdbResult<()> {
    lock_network().stop_network()
}

/// Disables the automatic network stop at process exit.
///
/// Every exit-time stop (thread-local guard and atexit hooks) becomes a no-op. Use it
/// when your application owns its shutdown sequence and calls [`stop_network`]
/// itself: the C API requires the network to be stopped and joined before a
/// normal process exit.
pub fn disable_stop_on_exit() {
    STOP_ON_EXIT.store(false, Ordering::Release);
}

/// Returns the error reported by the network event loop, if it stopped with one.
pub fn network_run_error() -> Option<FdbError> {
    match NETWORK_RUN_ERROR.load(Ordering::Acquire) {
        0 => None,
        code => Some(FdbError::from_code(code)),
    }
}

/// Starts the network with the default API version if nothing was booted yet.
///
/// Lazy-boot entry point used by `Database` constructors.
pub(crate) fn ensure_network_started() -> FdbResult<()> {
    lock_network().start_network()
}

/// A Builder with which different versions of the Fdb C API can be initialized
///
/// ```
/// foundationdb::api::FdbApiBuilder::default().build().expect("fdb api initialized");
/// ```
pub struct FdbApiBuilder {
    runtime_version: i32,
}

impl FdbApiBuilder {
    /// The version of run-time behavior the API is requested to provide.
    pub fn runtime_version(&self) -> i32 {
        self.runtime_version
    }

    /// Set the version of run-time behavior the API is requested to provide.
    ///
    /// Must be less than or equal to header_version, `foundationdb_sys::FDB_API_VERSION`, and should almost always be equal.
    /// Language bindings which themselves expose API versioning will usually pass the version requested by the application.
    pub fn set_runtime_version(mut self, version: i32) -> Self {
        self.runtime_version = version;
        self
    }

    /// Selects the foundationDB API version and returns a `NetworkBuilder`.
    ///
    /// Idempotent: re-selecting the version already in use is a no-op.
    ///
    /// # Errors
    ///
    /// - error 2201 (`api_version_already_set`) if a different version was selected
    ///   before
    /// - error 2203 (`api_version_not_supported`) if the installed libfdb_c does
    ///   not support the requested version
    pub fn build(self) -> FdbResult<NetworkBuilder> {
        lock_network().select_api_version(self.runtime_version)?;
        Ok(NetworkBuilder { _private: () })
    }
}

impl Default for FdbApiBuilder {
    fn default() -> Self {
        FdbApiBuilder {
            runtime_version: fdb_sys::FDB_API_VERSION as i32,
        }
    }
}

/// A Builder to configure network options and start the network event loop.
///
/// ```
/// use foundationdb::api::FdbApiBuilder;
///
/// let network_builder = FdbApiBuilder::default().build().expect("fdb api initialized");
/// network_builder.boot().expect("fdb network running");
/// ```
pub struct NetworkBuilder {
    _private: (),
}

impl NetworkBuilder {
    /// Set network options.
    ///
    /// # Errors
    ///
    /// - error 2009 (`network_already_setup`) once the network is running
    /// - error 2025 (`network_cannot_be_restarted`) once the network is stopped
    pub fn set_option(self, option: NetworkOption) -> FdbResult<Self> {
        lock_network().set_network_option(option)?;
        Ok(self)
    }

    /// Starts the FoundationDB network event loop in a dedicated thread.
    ///
    /// Safe and idempotent: booting an already running network is a no-op. The
    /// network runs until process exit, where it is stopped and the network thread
    /// joined, or until an explicit [`stop_network`] call.
    ///
    /// # Errors
    ///
    /// - error 2025 (`network_cannot_be_restarted`) if the network was stopped
    ///
    /// # Examples
    ///
    /// ```rust
    /// use foundationdb::api::FdbApiBuilder;
    ///
    /// let network_builder = FdbApiBuilder::default().build().expect("fdb api initialized");
    /// network_builder.boot().expect("fdb network running");
    /// // do some interesting things with the API...
    /// ```
    ///
    /// ```rust
    /// use foundationdb::api::FdbApiBuilder;
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let network_builder = FdbApiBuilder::default().build().expect("fdb api initialized");
    ///     network_builder.boot().expect("fdb network running");
    ///     // do some interesting things with the API...
    /// }
    /// ```
    pub fn boot(self) -> FdbResult<NetworkAutoStop> {
        lock_network().start_network()?;
        Ok(NetworkAutoStop { _private: () })
    }
}

/// Kept for backward compatibility: dropping it does nothing.
///
/// The network runs until process exit (where it is stopped and the network
/// thread joined), or until an explicit [`stop_network`] call.
pub struct NetworkAutoStop {
    _private: (),
}

impl Drop for NetworkAutoStop {
    fn drop(&mut self) {
        // Intentionally inert: the network is stopped at process exit (or by an
        // explicit stop_network()), never on guard drop. Stopping on drop would
        // make any later use of FoundationDB in the process fail with error 2025.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_max_api() {
        assert!(get_max_api_version() > 0);
    }

    // Only exercises the FFI-free arms of the transition table, on locally
    // constructed states: lib unit tests share one process, so they must neither
    // touch the NETWORK static nor select a real api version.
    #[test]
    fn test_transition_table_ffi_free_arms() {
        // ApiVersionSelected
        let mut state = NetworkLifecycle::ApiVersionSelected { api_version: 710 };
        state
            .select_api_version(710)
            .expect("same version must be idempotent");
        assert_eq!(
            state.select_api_version(620).unwrap_err().code(),
            ERR_API_VERSION_ALREADY_SET
        );
        state.stop_network().expect("stop before start must be Ok");
        assert!(matches!(
            state,
            NetworkLifecycle::Stopped { api_version: 710 }
        ));

        // Running (dummy finished thread; stop_network is never called on it)
        let mut state = NetworkLifecycle::Running {
            api_version: 710,
            handle: std::thread::spawn(|| {}),
        };
        state
            .start_network()
            .expect("start while running must be idempotent");
        assert_eq!(
            state.select_api_version(620).unwrap_err().code(),
            ERR_API_VERSION_ALREADY_SET
        );
        assert_eq!(
            state
                .set_network_option(NetworkOption::TraceEnable(String::new()))
                .unwrap_err()
                .code(),
            ERR_NETWORK_ALREADY_SETUP
        );

        // Stopped is terminal
        let mut state = NetworkLifecycle::Stopped { api_version: 710 };
        assert_eq!(
            state.start_network().unwrap_err().code(),
            ERR_NETWORK_CANNOT_BE_RESTARTED
        );
        assert_eq!(
            state
                .set_network_option(NetworkOption::TraceEnable(String::new()))
                .unwrap_err()
                .code(),
            ERR_NETWORK_CANNOT_BE_RESTARTED
        );
        state.stop_network().expect("stop must be idempotent");
        state
            .select_api_version(710)
            .expect("re-selecting the same version stays Ok once stopped");

        // Uninitialized
        let mut state = NetworkLifecycle::Uninitialized;
        state.stop_network().expect("stopping nothing must be Ok");
        assert!(matches!(state, NetworkLifecycle::Uninitialized));
        assert_eq!(
            state
                .set_network_option(NetworkOption::TraceEnable(String::new()))
                .unwrap_err()
                .code(),
            ERR_API_VERSION_UNSET
        );
    }
}
