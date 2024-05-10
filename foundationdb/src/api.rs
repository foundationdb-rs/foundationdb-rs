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

use std::panic;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use crate::options::NetworkOption;
use crate::{error, FdbResult};
use foundationdb_sys as fdb_sys;

/// Returns the max api version of the underlying Fdb C API Client
pub fn get_max_api_version() -> i32 {
    unsafe { fdb_sys::fdb_get_max_api_version() }
}

static VERSION_SELECTED: AtomicBool = AtomicBool::new(false);

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
        if VERSION_SELECTED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            panic!("the fdb select api version can only be run once per process");
        }
        error::eval(unsafe {
            fdb_sys::fdb_select_api_version_impl(
                self.runtime_version,
                fdb_sys::FDB_API_VERSION as i32,
            )
        }).map_err(|e| {
                // 2203: api_version_not_supported
                // generally mean the local libfdb doesn't support requested target version
                if e.code() == 2203 {
                    let max_api_version = unsafe { fdb_sys::fdb_get_max_api_version() };
                    if max_api_version < fdb_sys::FDB_API_VERSION as i32 {
                        println!("The version of FoundationDB binding requested '{}' is not supported", fdb_sys::FDB_API_VERSION);
                        println!("by the installed FoundationDB C library. Maximum supported version by the local library is {}", max_api_version);
                    }
                }
                e
            })?;

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
        unsafe { option.apply()? };
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
        unsafe { error::eval(fdb_sys::fdb_setup_network())? }

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
        let (runner, cond) = self.build()?;

        let net_thread = runner.spawn();

        let network = cond.wait();

        Ok(NetworkAutoStop {
            handle: Some(net_thread),
            network: Some(network),
        })
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
        self._run()
    }

    fn _run(self) -> FdbResult<()> {
        {
            let (lock, cvar) = &*self.cond;
            let mut started = lock.lock().unwrap();
            *started = true;
            // We notify the condvar that the value has changed.
            cvar.notify_one();
        }

        error::eval(unsafe { fdb_sys::fdb_run_network() })
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
        error::eval(unsafe { fdb_sys::fdb_stop_network() })
    }
}

/// Stop the associated `NetworkRunner` and thread if dropped
///
/// If trying to stop the FoundationDB run loop results in an error.
/// The error is printed in `stderr` and the process aborts.
///
/// # Panics
///
/// Panics if the network thread cannot be joined.
pub struct NetworkAutoStop {
    network: Option<NetworkStop>,
    handle: Option<std::thread::JoinHandle<()>>,
}
impl Drop for NetworkAutoStop {
    fn drop(&mut self) {
        if let Err(err) = self.network.take().unwrap().stop() {
            eprintln!("failed to stop network: {}", err);
            // Not aborting can probably cause undefined behavior
            std::process::abort();
        }
        self.handle
            .take()
            .unwrap()
            .join()
            .expect("failed to join fdb thread");
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
