// Copyright 2018 foundationdb-rs developers, https://github.com/Clikengo/foundationdb-rs/graphs/contributors
// Copyright 2013-2018 Apple, Inc and the FoundationDB project authors.
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! # FoundationDB API and Network Configuration
//!
//! This crate provides a safe wrapper around key FoundationDB C API calls, enabling effective management of the FoundationDB library lifecycle.
//!
//! ## Core API Coverage
//!
//! The following C API functionalities are wrapped safely:
//!
//! - [API Versioning](https://apple.github.io/foundationdb/api-c.html#api-versioning)
//! - [Network Management](https://apple.github.io/foundationdb/api-c.html#network)
//!
//! ## Initialization Sequence
//!
//! ### Initialization Sequence in C
//!
//! To correctly initialize the FoundationDB library (`libfdb`), the following C functions **must** be called in this order:
//!
//! 1. [fdb_select_api_version](https://apple.github.io/foundationdb/api-c.html#c.fdb_select_api_version)
//! 2. [fdb_database_set_option](https://apple.github.io/foundationdb/api-c.html#c.fdb_database_set_option)
//! 3. [fdb_setup_network](https://apple.github.io/foundationdb/api-c.html#c.fdb_setup_network)
//! 4. [fdb_run_network](https://apple.github.io/foundationdb/api-c.html#c.fdb_run_network) (in a dedicated thread).
//!
//! **Important:** Once the network thread is stopped, it cannot be restarted. None of the above functions can be called again.
//!
//! ### Initialization Sequence in Rust
//!
//! To handle these restrictions from `libfdb`, the initialization functions are encapsulated using [OnceLock] and other synchronization primitives:
//!
//! - [set_api_version] can be called multiple times, but it will only take effect once.
//! - [setup_network_thread] is safe to call repeatedly, initializing the network only once.
//! - Creating a new database instance via [crate::database::Database::new] ensures:
//!   - The API version is set (defaulting to [get_max_api_version] if unspecified).
//!   - The network thread is started, if not already running.
//!
//! The network thread is stored internally, ensuring it is managed properly.
//!
//! ### Initialization Options
//!
//! The library can be initialized in the following ways:
//!
//! - Using [crate::boot] for quick setup.
//! - Using [FdbApiBuilder] to configure options before starting the network thread.
//! - Manually, by calling [set_api_version], [setup_network_thread], and [spawn_network_thread] in sequence.
//!
//! ## Stopping Sequence
//!
//! At the end of a program using the FoundationDB client, call [stop_network] to release network resources and shut down the FoundationDB client cleanly.
//! **Failure to do so may result in undefined behavior.**
//!
//! ### Automated Network Cleanup
//!
//! Manually managing the network lifecycle can be difficult, especially in scenarios like testing given cargo's behavior.
//! The [NetworkAutoStop] struct simplifies this by automatically stopping the network when its last instance is dropped.
//! This utility ensures proper cleanup and reduces the risk of forgetting to call [stop_network], which is **undefined behavior**.
//!
//! ## Testing with FoundationDB
//!
//! Managing `libfdb` lifecycle during Rust tests presents unique challenges, especially when deciding when to call [stop_network].
//! Two recommended strategies are:
//!
//! - Obtain a [NetworkAutoStop] instance at the **start** of each test to ensure [stop_network] is invoked after all tests finish. An example is available [here](https://github.com/foundationdb-rs/foundationdb-rs/blob/main/foundationdb/tests/guard.rs).
//! - Use [cargo-nextest](https://nexte.st/), which runs each test in a separate process, isolating the FoundationDB lifecycle per test.

use crate::options::NetworkOption;
use crate::{error, FdbError, FdbResult};
use foundationdb_sys as fdb_sys;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::thread::JoinHandle;

// Used as a Lock to init once the version. The Option used inside allow to keep track
// of a potential error during init.
static INIT_VERSION_ONCE: OnceLock<Option<FdbError>> = OnceLock::new();
// Used as a Lock to init once the network. The Option used inside allow to keep track
// of a potential error during init.
static SETUP_NETWORK_ONCE: OnceLock<Option<FdbError>> = OnceLock::new();

// A global Mutex, used to keep track of the Network-thread's associated JoinHandle
static NETWORK_THREAD_HANDLER: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);
// This needs to be a Mutex and not an AtomicBool as it is required by the Condvar API
static NETWORK_STARTED_LOCK: Mutex<bool> = Mutex::new(false);

// Because the network-thread is started in another thread, we need to know when it is actually making progress
static NETWORK_STARTED_NOTIFIER: Condvar = Condvar::new();

// The network-thread cannot be restarted, so we need to keep track of the shutdown
static NETWORK_STOPPED: AtomicBool = AtomicBool::new(false);

// This usize is used as a Reference counter for the network handler. Once it is reaching 0,
// We can call stop_network.
#[doc(hidden)]
pub static NETWORK_STOP_HANDLER_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Returns the max api version of the underlying Fdb C API Client
pub fn get_max_api_version() -> i32 {
    unsafe { fdb_sys::fdb_get_max_api_version() }
}

/// Set the api version that will be used. **Must be called before any other API functions**.
/// Version must be less than or equal to FDB_API_VERSION (and should almost always be equal).
/// THis function is thread-safe, and can be called multiple times, as it is protected with a OnceLock.
/// Only the first version passed will be used. The error, if any, is kept throughout the different calls
pub fn set_api_version(version: i32) -> Result<(), FdbError> {
    match INIT_VERSION_ONCE.get_or_init(|| {
        match error::eval(unsafe {
            fdb_sys::fdb_select_api_version_impl(version, fdb_sys::FDB_API_VERSION as i32)
        }) {
            Ok(()) => None,
            Err(err) => {
                // api_version_not_supported
                if err.code() == 2203 {
                    let max_api_version = get_max_api_version();
                    if max_api_version < fdb_sys::FDB_API_VERSION as i32 {
                        eprintln!(
                            "The version of FoundationDB binding requested '{}' is not supported",
                            fdb_sys::FDB_API_VERSION
                        );
                        eprintln!("by the installed FoundationDB C library. Maximum supported version by the local library is {}", max_api_version);
                    }
                };
                Some(err)
            }
        }
    }) {
        None => Ok(()),
        Some(err) => Err(*err),
    }
}

/// Check if the API version is set. Will return fdb error 2200 for api_version_unset
pub fn check_api_version_set() -> Result<(), FdbError> {
    if INIT_VERSION_ONCE.get().is_some() {
        Ok(())
    } else {
        Err(FdbError::new(2200)) // api_version_unset
    }
}

/// Setup the network thread. Can be called multiple times.
pub fn setup_network_thread() -> Result<(), FdbError> {
    check_api_version_set()?;

    match SETUP_NETWORK_ONCE
        .get_or_init(|| unsafe { error::eval(fdb_sys::fdb_setup_network()).err() })
    {
        None => Ok(()),
        Some(err) => Err(*err),
    }
}

/// Check if the API version is set
pub fn is_network_setup() -> bool {
    SETUP_NETWORK_ONCE.get().is_some()
}

/// Spawn the network-thread if necessary. This will:
///   * check if api version is set,
///   * setup network thread if required
///   * start and store the network thread internally.
///
/// It is safe to call this function multiple time, even if the network thread have been stopped.
pub fn spawn_network_thread() -> Result<(), FdbError> {
    check_api_version_set()?;

    if !is_network_setup() {
        setup_network_thread()?;
    }

    if is_network_thread_running() {
        return Ok(());
    }

    if is_network_thread_stopped() {
        return Err(FdbError::new(2025)); // network_cannot_be_restarted
    }

    {
        let mut guard_thread = NETWORK_THREAD_HANDLER
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard_thread.is_none() {
            *guard_thread = Some(thread::spawn(run_network_thread));
        }
    }

    wait_for_network_thread();
    Ok(())
}

// main function that call fdb_run_network. Needs to be spawned in a thread
fn run_network_thread() {
    {
        let mut data: MutexGuard<bool> = NETWORK_STARTED_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *data = true;

        // We notify the condvar that the value has changed.
        NETWORK_STARTED_NOTIFIER.notify_all();
    }

    // Returns only when stopped
    let _ = error::eval(unsafe { fdb_sys::fdb_run_network() });

    {
        let mut data: MutexGuard<bool> = NETWORK_STARTED_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *data = false;

        NETWORK_STOPPED.store(true, Ordering::Release);
    }
}

// wait for `run_network_thread` to actually run.
fn wait_for_network_thread() {
    let mut started = NETWORK_STARTED_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    while !*started {
        started = NETWORK_STARTED_NOTIFIER.wait(started).unwrap();
    }
}

/// Returns a boolean if the network thread is running
pub fn is_network_thread_running() -> bool {
    let is_running = NETWORK_STARTED_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    *is_running
}

/// Return an Error if the network thread have been started then stopped, as it cannot be enabled back again.
pub fn is_network_thread_stopped() -> bool {
    NETWORK_STOPPED.load(Ordering::Acquire)
}

/// Stop the network thread used by the bindings. This **must** be called at the end of your program.
/// Once the network thread is stopped, it cannot be restarted, so you will need to restart your app.
pub fn stop_network() -> Result<(), FdbError> {
    if !is_network_thread_running() {
        return Ok(());
    }

    error::eval(unsafe { fdb_sys::fdb_stop_network() })?;

    if let Some(network_thread) = NETWORK_THREAD_HANDLER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
    {
        if let Err(err) = network_thread.join() {
            eprintln!(
                "Error during stop_network, cannot join network-thread: {:?}",
                err
            );
        }
    };

    Ok(())
}

/// Set network options. Must be call **before** setup_network.
pub fn set_network_option(option: NetworkOption) -> Result<(), FdbError> {
    if is_network_setup() {
        return Err(FdbError::new(2009)); // network_already_setup
    }
    unsafe { option.apply() }
}

/// A Builder with which different versions of the Fdb C API can be initialized.
///
/// For details on the boot and shutdown sequence and safety considerations, refer to the [crate::api] documentation.
pub struct FdbApiBuilder {
    runtime_version: i32,
}

impl FdbApiBuilder {
    /// The version of run-time behavior the API is requested to provide.
    pub fn runtime_version(&self) -> i32 {
        self.runtime_version
    }

    /// Create a new FdbApiBuilder for the given api version
    pub fn new(runtime_version: i32) -> Self {
        FdbApiBuilder { runtime_version }
    }

    /// Set the version of run-time behavior the API is requested to provide.
    ///
    /// Must be less than or equal to header_version, `foundationdb_sys::FDB_API_VERSION`, and should almost always be equal.
    /// Language bindings which themselves expose API versioning will usually pass the version requested by the application.
    pub fn set_runtime_version(mut self, version: i32) -> Self {
        self.runtime_version = version;
        self
    }

    /// Initialize the foundationDB API and returns a `NetworkBuilder`
    /// This function is now thread-safe
    pub fn build(self) -> FdbResult<NetworkBuilder> {
        set_api_version(self.runtime_version)?;
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

/// A Builder with which the foundationDB network event loop can be created
///
/// The foundationDB Network event loop can only be run once.
///
/// ```
/// use foundationdb::api::FdbApiBuilder;
///
/// let network_builder = FdbApiBuilder::default()
///     .build()
///     .expect("fdb api initialized");
/// let guard = unsafe { network_builder.boot() }.expect("could not boot");
/// // do some work with foundationDB, then drop the guard
/// drop(guard);
/// ```
pub struct NetworkBuilder {
    _private: (),
}

impl NetworkBuilder {
    /// Set network options.
    pub fn set_option(self, option: NetworkOption) -> FdbResult<Self> {
        set_network_option(option)?;
        Ok(self)
    }

    /// Finalizes the initialization of the Network and returns a way to run/wait/stop the
    /// FoundationDB run loop. The call is now an noop, it has been kept for compatibility reason
    pub fn build(self) -> FdbResult<()> {
        Ok(())
    }

    /// Starts the FoundationDB network thread in a dedicated thread.
    /// This finish initializing the FoundationDB Client API.
    ///
    /// You **must** call `stop_network` at the end of your program, or use the Drop feature of [NetworkAutoStop]
    ///
    /// ## Safety
    ///
    /// This function is `unsafe` because Rust cannot enforce
    /// that `stop_network` will be called at the end of your program.
    /// Using this function incorrectly may introduce undefined behavior.
    pub unsafe fn boot(self) -> FdbResult<NetworkAutoStop> {
        spawn_network_thread()?;
        Ok(NetworkAutoStop::new())
    }
}

#[allow(clippy::test_attr_in_doctest)]
/// A utility for automatically stopping the FoundationDB network.
///
/// `NetworkAutoStop` simplifies the management of the FoundationDB network lifecycle by automatically
/// stopping the network when its last instance is dropped. This reduces the risk of undefined behavior
/// caused by forgetting to call [`stop_network`] manually.
///
/// # Key Features
///
/// - Ensures [`stop_network`] is called when all instances of `NetworkAutoStop` are dropped.
/// - Reference counting ensures that the network is only stopped once, even if multiple `NetworkAutoStop`
///   instances are created or cloned.
///
/// # Usage
///
/// `NetworkAutoStop` is typically used during testing or in situations where manual network management
/// would be error-prone. For example:
///
/// ```rust
/// #[test]
/// fn test_network_auto_stop() {
///     // Acquire a `NetworkAutoStop` instance at the start of the test
///     let guard = NetworkAutoStop::new();
///
///     // Initialize FoundationDB normally, skipping the auto-stop functionality provided by `boot`
///     foundationdb::boot().expect("failed to initialize FoundationDB");
///
///     // Perform operations with FoundationDB
///
///     // Dropping the `guard` ensures the network is stopped automatically when no other instances exist
///     drop(guard);
/// }
/// ```
pub struct NetworkAutoStop {
    _private: (),
}

impl Default for NetworkAutoStop {
    fn default() -> Self {
        Self::new()
    }
}

impl NetworkAutoStop {
    pub fn new() -> Self {
        NETWORK_STOP_HANDLER_COUNTER.fetch_add(1, Ordering::Acquire);
        Self { _private: () }
    }

    /// Attempts to stop the FoundationDB network if it is running.
    ///
    /// This method decreases the internal reference counter tracking active network users. If the counter
    /// reaches zero and the network thread is still running, it invokes [`stop_network`] to shut down the
    /// FoundationDB network cleanly.
    pub fn stop_network(&self) -> Result<(), FdbError> {
        let previous_count = NETWORK_STOP_HANDLER_COUNTER.fetch_sub(1, Ordering::Acquire);
        if previous_count == 1 && is_network_thread_running() {
            stop_network()?
        }
        Ok(())
    }
}

impl Clone for NetworkAutoStop {
    fn clone(&self) -> Self {
        Self::new()
    }
}

impl Drop for NetworkAutoStop {
    fn drop(&mut self) {
        // Because Drop should not fail, we are printing the error instead of calling expect or equivalent
        if let Err(err) = self.stop_network() {
            eprintln!("could not stop_network during Drop: '{err}'");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_max_api() {
        assert!(get_max_api_version() > 0);
    }

    #[test]
    fn test_clone_network_stop() {
        {
            let _n1 = NetworkAutoStop::new();
            NETWORK_STOP_HANDLER_COUNTER.load(Ordering::Acquire);
            let _n2 = _n1.clone();
            {
                let _n3 = NetworkAutoStop::new();

                assert_eq!(3, NETWORK_STOP_HANDLER_COUNTER.load(Ordering::Acquire));
            }
        }
        assert_eq!(0, NETWORK_STOP_HANDLER_COUNTER.load(Ordering::Acquire));
    }
}
