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

use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use crate::options::NetworkOption;
use crate::{error, FdbError, FdbResult};
use foundationdb_sys as fdb_sys;

#[derive(Debug)]
pub(crate) enum GlobalState {
    Uninitialized,
    ApiVersionSelected {
        api_version: i32,
    },
    // this state is only used for unmanaged networks
    NetworkSetup {
        api_version: i32,
    },
    Running {
        api_version: i32,
        handle: Option<std::thread::JoinHandle<()>>,
        count: usize,
    },
    Stopped,
}

pub struct NetworkGuard {
    global: bool,
}
pub type NetworkAutoStop = NetworkGuard;

impl Drop for NetworkGuard {
    fn drop(&mut self) {
        if !GLOBAL_STATE.lock().unwrap().stop() && self.global {
            panic!("Some guards were not dropped, the network is still running, aborting");
        }
    }
}

#[cfg(feature = "global_guard")]
unsafe extern "C" {
    fn atexit(cb: extern "C" fn()) -> i32;
}

impl GlobalState {
    fn set_api_version(&mut self, version: i32) -> FdbResult<()> {
        println!("GlobalState::set_api_version({version})");
        match self {
            GlobalState::Uninitialized => {
                let result = error::eval(unsafe {
                    fdb_sys::fdb_select_api_version_impl(version, fdb_sys::FDB_API_VERSION as i32)
                });
                match result {
                    Ok(()) => {
                        *self = GlobalState::ApiVersionSelected {
                            api_version: version,
                        }
                    }
                    // 2203: api_version_not_supported
                    // generally mean the local libfdb doesn't support requested target version
                    Err(e) if e.code() == 2203 => {
                        let max_api_version = unsafe { fdb_sys::fdb_get_max_api_version() };
                        if max_api_version < fdb_sys::FDB_API_VERSION as i32 {
                            eprintln!("The version of FoundationDB binding requested '{}' is not supported", fdb_sys::FDB_API_VERSION);
                            eprintln!("by the installed FoundationDB C library. Maximum supported version by the local library is {max_api_version}");
                        }
                    }
                    Err(_) => {}
                }
                result
            }
            GlobalState::ApiVersionSelected { api_version }
            | GlobalState::Running { api_version, .. } => {
                eprintln!("Warning: fdb api version was already set");
                if version != *api_version {
                    eprintln!(
                        "requested fdb api version: {version}, previously set: {api_version}"
                    );
                    return Err(FdbError::from_code(2201));
                }
                Ok(())
            }
            GlobalState::Stopped => {
                // network_cannot_be_restarted
                Err(FdbError::from_code(2025))
            }
            GlobalState::NetworkSetup { .. } => unreachable!(),
        }
    }
    fn set_option(&mut self, option: NetworkOption) -> FdbResult<()> {
        match self {
            GlobalState::Uninitialized => unreachable!(),
            GlobalState::ApiVersionSelected { .. } => unsafe { option.apply() },
            GlobalState::NetworkSetup { .. } | GlobalState::Running { .. } => {
                // network_already_setup
                Err(FdbError::from_code(2009))
            }
            GlobalState::Stopped => {
                // network_cannot_be_restarted
                Err(FdbError::from_code(2025))
            }
        }
    }
    fn boot(&mut self) -> FdbResult<NetworkAutoStop> {
        println!("GlobalState::boot");
        match self {
            GlobalState::Uninitialized => unreachable!(),
            GlobalState::ApiVersionSelected { api_version, .. } => {
                error::eval(unsafe { fdb_sys::fdb_setup_network() })?;
                let handle = thread::Builder::new()
                    .name("fdb-network-thread".to_owned())
                    .spawn(move || {
                        error::eval(unsafe { fdb_sys::fdb_run_network() }).expect("network start")
                    })
                    .expect("Could not start network thread");
                let (count, global) = if cfg!(feature = "global_guard") {
                    eprintln!("Warning: with feature `global_guard` calling boot is unecessary");
                    println!("GlobalState::boot: register atexit hook");
                    unsafe { atexit(GlobalState::exit) };
                    (2, false)
                } else {
                    (1, true)
                };
                *self = GlobalState::Running {
                    api_version: *api_version,
                    handle: Some(handle),
                    count,
                };
                Ok(NetworkGuard { global })
            }
            GlobalState::NetworkSetup { .. } => {
                panic!("You tried to use boot after NetworkBuilder::build but before NetworkRunner::run")
            }
            GlobalState::Running { count, .. } => {
                *count += 1;
                println!("GlobalState::boot: {count} guards alive");
                Ok(NetworkGuard { global: false })
            }
            GlobalState::Stopped => {
                // network_cannot_be_restarted
                Err(FdbError::from_code(2025))
            }
        }
    }
    pub(crate) fn start(&mut self) -> FdbResult<NetworkGuard> {
        match self {
            GlobalState::Uninitialized => {
                self.set_api_version(fdb_sys::FDB_API_VERSION as i32)?;
                self.start()
            }
            GlobalState::ApiVersionSelected { api_version } => {
                error::eval(unsafe { fdb_sys::fdb_setup_network() })?;
                let handle = thread::Builder::new()
                    .name("fdb-network-thread".to_owned())
                    .spawn(move || {
                        error::eval(unsafe { fdb_sys::fdb_run_network() }).expect("network start")
                    })
                    .expect("Could not start network thread");
                let count = if cfg!(feature = "global_guard") {
                    println!("GlobalState::start: register atexit hook");
                    unsafe { atexit(GlobalState::exit) };
                    2
                } else {
                    1
                };
                *self = GlobalState::Running {
                    api_version: *api_version,
                    handle: Some(handle),
                    count,
                };
                Ok(NetworkGuard { global: false })
            }
            GlobalState::NetworkSetup { .. } => {
                panic!("Database::new was called but the network is unmanaged and the network was not started");
            }
            GlobalState::Running { count, .. } => {
                *count += 1;
                println!("GlobalState::start: {count} guards alive");
                Ok(NetworkGuard { global: false })
            }
            GlobalState::Stopped => {
                // network_cannot_be_restarted
                Err(FdbError::from_code(2025))
            }
        }
    }
    fn stop(&mut self) -> bool {
        match self {
            // stop is private and can only be called by NetworkGuard::drop
            GlobalState::Uninitialized
            | GlobalState::ApiVersionSelected { .. }
            | GlobalState::NetworkSetup { .. }
            | GlobalState::Running { count: 0, .. }
            | GlobalState::Stopped => unreachable!("Tried to stop when GLOBAL_STATE={self:?}"),
            GlobalState::Running { count: 1, .. } => {
                let GlobalState::Running { handle, .. } =
                    core::mem::replace(self, GlobalState::Stopped)
                else {
                    unreachable!()
                };
                println!("GlobaState::Stop: no guard alive: killing");
                let result = error::eval(unsafe { fdb_sys::fdb_stop_network() });
                if let Err(error) = result {
                    eprintln!("failed to stop network: {error}");
                    // Not aborting can probably cause undefined behavior
                    std::process::abort();
                }
                if let Some(handle) = handle {
                    handle.join().expect("failed to join fdb thread");
                }
                true
            }
            GlobalState::Running { count, .. } => {
                *count -= 1;
                println!("GlobaState::Stop: still alive: {count}");
                false
            }
        }
    }
    extern "C" fn exit() {
        println!("GlobaState::exit");
        drop(NetworkGuard { global: true })
    }
}

impl GlobalState {
    fn setup_unmanaged(&mut self) -> FdbResult<()> {
        println!("GlobalState::setup_unamanged");
        match self {
            GlobalState::ApiVersionSelected { api_version } => {
                let result = error::eval(unsafe { fdb_sys::fdb_setup_network() });
                if result.is_ok() {
                    *self = GlobalState::NetworkSetup {
                        api_version: *api_version,
                    }
                }
                result
            }
            _ => unreachable!(),
        }
    }
    fn start_unmanaged(&mut self) -> FdbResult<()> {
        match self {
            GlobalState::NetworkSetup { api_version } => {
                if cfg!(feature = "global_guard") {
                    eprint!("Warning: feature `global_guard` is ignored in unmanaged mode");
                }
                *self = GlobalState::Running {
                    api_version: *api_version,
                    handle: None,
                    count: 1,
                };
                error::eval(unsafe { fdb_sys::fdb_run_network() })
            }
            _ => unreachable!(),
        }
    }
}

pub(crate) static GLOBAL_STATE: Mutex<GlobalState> = Mutex::new(GlobalState::Uninitialized);

/// Returns the max api version of the underlying Fdb C API Client
pub fn get_max_api_version() -> i32 {
    unsafe { fdb_sys::fdb_get_max_api_version() }
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
    ///
    /// # Panics
    ///
    /// This function will panic if called more than once
    pub fn build(self) -> FdbResult<NetworkBuilder> {
        GLOBAL_STATE
            .lock()
            .unwrap()
            .set_api_version(self.runtime_version)?;
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
/// let guard = unsafe { network_builder.boot() };
/// // do some work with foundationDB
/// drop(guard);
/// ```
pub struct NetworkBuilder {
    _private: (),
}

impl NetworkBuilder {
    /// Set network options.
    pub fn set_option(self, option: NetworkOption) -> FdbResult<Self> {
        GLOBAL_STATE.lock().unwrap().set_option(option)?;
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
    ///
    /// # Example
    ///
    /// ```
    /// use foundationdb::api::FdbApiBuilder;
    ///
    /// let network_builder = FdbApiBuilder::default().build().expect("fdb api initialized");
    /// let (runner, cond) = network_builder.build().expect("fdb network runners");
    ///
    /// let net_thread = std::thread::spawn(move || {
    ///     unsafe { runner.run() }.expect("failed to run");
    /// });
    ///
    /// // Wait for the foundationDB network thread to start
    /// let fdb_network = cond.wait();
    ///
    /// // do some work with foundationDB, if a panic occur you still **MUST** catch it and call
    /// // fdb_network.stop();
    ///
    /// // You **MUST** call fdb_network.stop() before the process exit
    /// fdb_network.stop().expect("failed to stop network");
    /// net_thread.join().expect("failed to join fdb thread");
    /// ```
    #[allow(clippy::mutex_atomic)]
    pub fn build(self) -> FdbResult<(NetworkRunner, NetworkWait)> {
        GLOBAL_STATE.lock().unwrap().setup_unmanaged()?;

        let cond = Arc::new((Mutex::new(false), Condvar::new()));
        Ok((NetworkRunner { cond: cond.clone() }, NetworkWait { cond }))
    }

    /// Starts the FoundationDB run loop in a dedicated thread.
    /// This finish initializing the FoundationDB Client API and can only be called once per process.
    ///
    /// # Returns
    ///
    /// A `NetworkAutoStop` handle which must be dropped before the program exits.
    ///
    /// # Safety
    ///
    /// You *MUST* ensure `drop` is called on the returned object before the program exits.
    /// This is not required if the program is aborted.
    ///
    /// This method used to be safe in version `0.4`. But because `drop` on the returned object
    /// might not be called before the program exits, it was found unsafe.
    ///
    /// # Panics
    ///
    /// Panics if the dedicated thread cannot be spawned or the internal condition primitive is
    /// poisonned.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use foundationdb::api::FdbApiBuilder;
    ///
    /// let network_builder = FdbApiBuilder::default().build().expect("fdb api initialized");
    /// let network = unsafe { network_builder.boot() };
    /// // do some interesting things with the API...
    /// drop(network);
    /// ```
    ///
    /// ```rust
    /// use foundationdb::api::FdbApiBuilder;
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let network_builder = FdbApiBuilder::default().build().expect("fdb api initialized");
    ///     let network = unsafe { network_builder.boot() };
    ///     // do some interesting things with the API...
    ///     drop(network);
    /// }
    /// ```
    pub unsafe fn boot(self) -> FdbResult<NetworkAutoStop> {
        GLOBAL_STATE.lock().unwrap().boot()
    }
}

/// A foundationDB network event loop runner
///
/// Most of the time you should never need to use this directly and use `boot()`.
pub struct NetworkRunner {
    cond: Arc<(Mutex<bool>, Condvar)>,
}

impl NetworkRunner {
    /// Start the foundationDB network event loop in the current thread.
    ///
    /// # Safety
    ///
    /// This method is unsafe because you **MUST** call the `stop` method on the
    /// associated `NetworkStop` before the program exit.
    ///
    /// This will only returns once the `stop` method on the associated `NetworkStop`
    /// object is called or if the foundationDB event loop return an error.
    pub unsafe fn run(self) -> FdbResult<()> {
        {
            let (lock, cvar) = &*self.cond;
            let mut started = lock.lock().unwrap();
            *started = true;
            // We notify the condvar that the value has changed.
            cvar.notify_one();
        }

        GLOBAL_STATE.lock().unwrap().start_unmanaged()
    }

    unsafe fn spawn(self) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            self.run().expect("failed to run network thread");
        })
    }
}

/// A condition object that can wait for the associated `NetworkRunner` to actually run.
///
/// Most of the time you should never need to use this directly and use `boot()`.
pub struct NetworkWait {
    cond: Arc<(Mutex<bool>, Condvar)>,
}

impl NetworkWait {
    /// Wait for the associated `NetworkRunner` to actually run.
    ///
    /// # Panics
    ///
    /// Panics if the internal lock cannot is poisoned
    pub fn wait(self) -> NetworkStop {
        // Wait for the thread to start up.
        {
            let (lock, cvar) = &*self.cond;
            let mut started = lock.lock().unwrap();
            while !*started {
                started = cvar.wait(started).unwrap();
            }
        }

        NetworkStop { _private: () }
    }
}

/// Allow to stop the associated and running `NetworkRunner`.
///
/// Most of the time you should never need to use this directly and use `boot()`.
pub struct NetworkStop {
    _private: (),
}

impl NetworkStop {
    /// Signals the event loop invoked by `Network::run` to terminate.
    pub fn stop(self) -> FdbResult<()> {
        drop(NetworkGuard { global: true });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_max_api() {
        assert!(get_max_api_version() > 0);
    }
}
