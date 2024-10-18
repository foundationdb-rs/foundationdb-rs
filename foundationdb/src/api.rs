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

use crate::options::NetworkOption;
use crate::{error, FdbError, FdbResult};
use foundationdb_sys as fdb_sys;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::thread::JoinHandle;

static INIT_VERSION_ONCE: OnceLock<Option<FdbError>> = OnceLock::new();
static SETUP_NETWORK_ONCE: OnceLock<Option<FdbError>> = OnceLock::new();
static NETWORK_THREAD_HANDLER: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);
// this needs to be a Mutex and not an AtomicBool as it is required by the Condvar API
static NETWORK_STARTED_LOCK: Mutex<bool> = Mutex::new(false);
static NETWORK_STARTED_NOTIFIER: Condvar = Condvar::new();
static NETWORK_STOPPED: AtomicBool = AtomicBool::new(false);
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
        Some(err) => Err(err.clone()),
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
fn setup_network_thread() -> Result<(), FdbError> {
    check_api_version_set()?;

    match SETUP_NETWORK_ONCE
        .get_or_init(|| unsafe { error::eval(fdb_sys::fdb_setup_network()).err() })
    {
        None => Ok(()),
        Some(err) => Err(err.clone()),
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
/// It is safe to call this function multiple time, even if the network thread have been stopped.
pub fn spawn_network_thread_if_needed() -> Result<(), FdbError> {
    check_api_version_set()?;

    if !is_network_setup() {
        setup_network_thread()?;
    }

    if is_network_thread_running() {
        return Ok(());
    }

    is_network_thread_stopped()?;

    {
        let mut guard_thread = NETWORK_THREAD_HANDLER
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard_thread = Some(thread::spawn(run_network_thread));
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
pub fn is_network_thread_stopped() -> Result<(), FdbError> {
    if NETWORK_STOPPED.load(Ordering::Acquire) {
        Ok(())
    } else {
        Err(FdbError::new(2025)) // network_cannot_be_restarted
    }
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
        network_thread
            .join()
            .expect("could not join network_thread");
    };

    Ok(())
}

/// Set network options. Must be call **before** setup_network.
pub fn set_network_option(option: NetworkOption) -> Result<(), FdbError> {
    unsafe { option.apply() }
}

/// A Builder with which different versions of the Fdb C API can be initialized
///
/// The foundationDB C API can only be initialized once.
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
/// let network_builder = FdbApiBuilder::default().build().expect("fdb api initialized");
/// network_builder.boot().expect("could not boot");
/// // do some work with foundationDB
/// foundationdb::api::stop_network().expect("could not stop network");
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
    /// FoundationDB run loop.
    ///
    /// It's not recommended to use this method directly, you probably want the `boot()` method.
    ///
    /// In order to start the network you have to:
    ///  - call the unsafe `NetworkRunner::run()` method, most likely in a dedicated thread
    ///  - wait for the thread to start `NetworkWait::wait`
    ///
    /// In order for the sequence to be safe, you **MUST** as stated in the `NetworkRunner::run()` method
    /// ensure that `NetworkStop::stop()` is called before the process exit.
    /// Aborting the process is still safe.
    /// ```
    pub fn build(self) -> FdbResult<()> {
        Ok(())
    }

    /// Starts the FoundationDB network thread in a dedicated thread.
    /// This finish initializing the FoundationDB Client API.
    ///
    /// You **must** call `stop_network` at the end of your program.
    pub fn boot(self) -> FdbResult<NetworkAutoStop> {
        spawn_network_thread_if_needed()?;
        Ok(NetworkAutoStop::new())
    }
}

/// Allow to stop the associated and running `NetworkRunner`.
///
/// Most of the time you should never need to use this directly and use `boot()`.
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
}

impl Clone for NetworkAutoStop {
    fn clone(&self) -> Self {
        Self::new()
    }
}

impl Drop for NetworkAutoStop {
    fn drop(&mut self) {
        let previous_count = NETWORK_STOP_HANDLER_COUNTER.fetch_sub(1, Ordering::Acquire);
        dbg!(previous_count);
        if dbg!(previous_count == 1 && is_network_thread_running()) {
            stop_network().expect("could not stop network");
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
        let _n1 = NetworkAutoStop::new();
        dbg!(NETWORK_STOP_HANDLER_COUNTER.load(Ordering::Acquire));
        let _n2 = _n1.clone();
        {
            let _n3 = NetworkAutoStop::new();

            assert_eq!(3, NETWORK_STOP_HANDLER_COUNTER.load(Ordering::Acquire));
        }
    }
}
