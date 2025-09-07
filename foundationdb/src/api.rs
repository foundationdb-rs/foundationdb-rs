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
//! # FoundationDB Network Booting Process
//!
//! FoundationDB requires a specific initialization sequence that can only happen once per process.
//! This module provides two approaches to handle this requirement safely and ergonomically.
//!
//! ## The Underlying Problem
//!
//! The FoundationDB C API enforces strict one-time initialization constraints:
//! - **API Version Selection**: `fdb_select_api_version_impl()` can only be called once per process
//! - **Network Setup**: `fdb_setup_network()` can only be called once per process
//! - **Network Thread**: The network thread should only be started once per process
//! - **Network Stop**: `fdb_stop_network()` can only be called once, and network cannot be restarted
//!
//! Violating these constraints results in errors like:
//! - Error 2201: "API version may be set only once"
//! - Error 2009: "Network can be configured only once"
//! - Error 2025: "Network can only be started once"
//!
//! ## Two Ways to Initialize FoundationDB
//!
//! ### 1. Automatic Management with `Database::new()` (Recommended)
//!
//! The simplest and recommended approach is to use `Database::new()` directly:
//!
//! ```rust
//! use foundationdb::Database;
//!
//! async fn example() -> foundationdb::FdbResult<()> {
//!     // Network is automatically initialized with default settings
//!     let db = Database::new(None)?;
//!     
//!     // Use the database...
//!     // Network will be automatically stopped when the last Database is dropped
//!     Ok(())
//! }
//! ```
//!
//! **Benefits:**
//! - Zero configuration required
//! - Thread-safe and race-condition free
//! - Automatic cleanup when no longer needed
//! - Works seamlessly in parallel tests
//! - Uses default API version and network options
//!
//! ### 2. Custom Configuration with Builder Pattern
//!
//! For applications that need custom API versions or network options:
//!
//! ```rust
//! use foundationdb::api::FdbApiBuilder;
//! use foundationdb::options::NetworkOption;
//! use foundationdb::Database;
//!
//! async fn example_with_custom_config() -> foundationdb::FdbResult<()> {
//!     // Step 1: Configure API version and network options BEFORE creating Database
//!     let _network_builder = FdbApiBuilder::default()
//!         .set_runtime_version(710)
//!         .build()?
//!         .set_option(NetworkOption::ExternalClientDirectory("/path/to/lib".to_string()))?;
//!         // Note: Don't call .build() on the NetworkBuilder for automatic management
//!     
//!     // Step 2: Create Database - will use the custom configuration
//!     let db = Database::new(None)?;
//!     
//!     // Use the database with custom settings...
//!     Ok(())
//! }
//! ```
//!
//! **Important:** Configuration must happen before the first `Database` is created. Once the network
//! is initialized, configuration cannot be changed.
//!
//! ## Unified Implementation Architecture
//!
//! Both approaches use the same underlying implementation to prevent race conditions:
//!
//! 1. **`ensure_api_version_set()`** - Protected by `std::sync::Once`, calls `fdb_select_api_version_impl()`
//! 2. **`ensure_network_setup()`** - Protected by `std::sync::Once`, calls `fdb_setup_network()`
//! 3. **`ensure_network_thread_running()`** - Protected by `std::sync::Once`, spawns network thread
//! 4. **`stop_network_once()`** - Protected by `std::sync::Once`, calls `fdb_stop_network()`
//!
//! This ensures that:
//! - Each FoundationDB C API call happens exactly once per process
//! - Both manual and automatic management coordinate safely
//! - No race conditions occur between concurrent initializations
//! - Reference counting tracks active `Database` instances
//!
//! ## Network Lifecycle
//!
//! - **Startup**: Network starts automatically when the first `Database` is created
//! - **Runtime**: Multiple `Database` instances can coexist safely
//! - **Shutdown**: Network stops automatically when the last `Database` is dropped
//! - **Restart**: Once stopped, the network **cannot** be restarted (FoundationDB limitation)
//!
//! ## Legacy Compatibility
//!
//! The legacy `boot()` function is still supported but deprecated:
//!
//! ```rust
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // Still works but deprecated - use Database::new() instead
//! let _guard = unsafe { foundationdb::boot()? };
//! let db = foundationdb::Database::new(None)?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Error Handling
//!
//! Common initialization errors:
//! - **2203**: API version not supported by installed FoundationDB
//! - **2025**: Network already stopped (cannot restart)
//! - **2201**: API version already set with different value
//!
//! ## Simulation Mode Support
//!
//! For FoundationDB simulation testing, use `Database::new_from_pointer()` which doesn't
//! participate in network management (the simulation runtime manages the network externally).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex, Once};
use std::thread::{self, JoinHandle};

use crate::options::NetworkOption;
use crate::{error, FdbResult};

// FoundationDB error codes used in network management
pub mod error_codes {
    /// Network can only be started once / Network already stopped
    pub const NETWORK_ALREADY_STOPPED: i32 = 2025;
    /// API version not supported by installed FoundationDB
    pub const API_VERSION_NOT_SUPPORTED: i32 = 2203;
}
use foundationdb_sys as fdb_sys;

/// Returns the max api version of the underlying Fdb C API Client
pub fn get_max_api_version() -> i32 {
    unsafe { fdb_sys::fdb_get_max_api_version() }
}

// Global Once primitives for each step of the booting sequence
static API_VERSION_SETUP: Once = Once::new();
static NETWORK_SETUP: Once = Once::new();
static THREAD_START: Once = Once::new();
static NETWORK_STOP: Once = Once::new();

static VERSION_SELECTED: AtomicBool = AtomicBool::new(false);

// Network state management
struct NetworkState {
    ref_count: usize,
    thread_handle: Option<JoinHandle<()>>,
    stop_requested: bool,  // Prevent multiple stop attempts
    network_running: bool, // Track if network is actually running
}

/// Global network state using LazyLock for thread-safe initialization.
///
/// # Memory Usage Notes
///
/// This static variable persists for the entire process lifetime and contains:
/// - Reference counter (8 bytes on 64-bit systems)  
/// - Optional thread handle (~24 bytes when Some)
/// - Boolean flags (2 bytes)
/// - Mutex overhead (~40 bytes on Linux)
/// - Arc overhead (~16 bytes)
///
/// Total memory usage: ~90 bytes per process (acceptable overhead)
///
/// The LazyLock ensures this is only allocated when first FoundationDB operation occurs,
/// not at process startup. Memory is never freed (by design - global process state).
static NETWORK_STATE: LazyLock<Arc<Mutex<NetworkState>>> = LazyLock::new(|| {
    Arc::new(Mutex::new(NetworkState {
        ref_count: 0,
        thread_handle: None,
        stop_requested: false,
        network_running: false,
    }))
});

/// Store results of one-time operations using LazyLock for thread-safe initialization.
///
/// # Memory Usage Notes
///
/// These static variables persist for the entire process lifetime:
/// - Each LazyLock<Mutex<Option<FdbResult<()>>>> uses ~48 bytes
/// - Total for all three: ~144 bytes per process
///
/// Memory is allocated only when first needed and never freed (by design).
/// This is acceptable overhead for ensuring thread-safe initialization.
static API_VERSION_RESULT: LazyLock<Mutex<Option<FdbResult<()>>>> =
    LazyLock::new(|| Mutex::new(None));
static NETWORK_SETUP_RESULT: LazyLock<Mutex<Option<FdbResult<()>>>> =
    LazyLock::new(|| Mutex::new(None));
static THREAD_START_RESULT: LazyLock<Mutex<Option<FdbResult<()>>>> =
    LazyLock::new(|| Mutex::new(None));

// Core functions (each protected by Once)

/// Ensures the FoundationDB API version is set exactly once.
///
/// This function is thread-safe and will only call `fdb_select_api_version_impl`
/// once per process, even when called concurrently from multiple threads.
/// Subsequent calls return the cached result.
#[cfg_attr(feature = "trace", tracing::instrument(level = "info"))]
fn ensure_api_version_set(runtime_version: i32) -> FdbResult<()> {
    API_VERSION_SETUP.call_once(|| {
        #[cfg(feature = "trace")]
        tracing::info!("Setting API version {}", runtime_version);
        let result = (|| -> FdbResult<()> {
            // Set the atomic flag first to prevent double initialization
            if VERSION_SELECTED
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                // Already set by another thread, this shouldn't happen in call_once
                #[cfg(feature = "trace")]
                tracing::debug!("API version already set by another thread");
                return Ok(());
            }

            error::eval(unsafe {
                fdb_sys::fdb_select_api_version_impl(
                    runtime_version,
                    fdb_sys::FDB_API_VERSION as i32,
                )
            }).inspect_err(|e| {
                #[cfg(feature = "trace")]
                tracing::error!("API version selection failed: {}", e);
                if e.code() == error_codes::API_VERSION_NOT_SUPPORTED {
                    let max_api_version = unsafe { fdb_sys::fdb_get_max_api_version() };
                    if max_api_version < fdb_sys::FDB_API_VERSION as i32 {
                        println!("The version of FoundationDB binding requested '{}' is not supported", fdb_sys::FDB_API_VERSION);
                        println!("by the installed FoundationDB C library. Maximum supported version by the local library is {max_api_version}");
                    }
                }
            })
        })();
        #[cfg(feature = "trace")]
        tracing::debug!("API version setup completed successfully: {}", result.is_ok());
        *API_VERSION_RESULT.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
    });

    // Return the stored result - this is guaranteed to exist after call_once completes
    let result = API_VERSION_RESULT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .expect("API version result should be set after call_once")
        .clone();
    result
}

/// Ensures the FoundationDB network is set up exactly once.
///
/// This function is thread-safe and will only call `fdb_setup_network` once per process.
/// Must be called after API version is set and before starting the network thread.
#[cfg_attr(feature = "trace", tracing::instrument(level = "info"))]
fn ensure_network_setup() -> FdbResult<()> {
    NETWORK_SETUP.call_once(|| {
        #[cfg(feature = "trace")]
        tracing::info!("Setting up FoundationDB network");
        let result = unsafe { error::eval(fdb_sys::fdb_setup_network()) };
        #[cfg(feature = "trace")]
        tracing::debug!("Network setup completed: {}", result.is_ok());
        *NETWORK_SETUP_RESULT
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(result);
    });

    let result = NETWORK_SETUP_RESULT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .expect("Network setup result should be set after call_once")
        .clone();
    result
}

/// Ensures the FoundationDB network thread is started exactly once.
///
/// This function is thread-safe and will spawn the network thread only once per process.
/// The thread runs `fdb_run_network` and handles all FoundationDB network communication.
/// Must be called after network setup is complete.
#[cfg_attr(feature = "trace", tracing::instrument(level = "info"))]
fn ensure_network_thread_running() -> FdbResult<()> {
    THREAD_START.call_once(|| {
        #[cfg(feature = "trace")]
        tracing::info!("Starting FoundationDB network thread");
        let result = (|| -> FdbResult<()> {
            // Create condition variable for thread startup
            let cond = Arc::new((Mutex::new(false), Condvar::new()));
            let cond_clone = cond.clone();

            // Spawn the network thread
            let handle = thread::spawn(move || {
                #[cfg(feature = "trace")]
                tracing::info!("FoundationDB network thread started");
                // Signal that thread has started
                {
                    let (lock, cvar) = &*cond_clone;
                    let mut started = lock.lock().unwrap_or_else(|e| e.into_inner());
                    *started = true;
                    cvar.notify_one();
                }

                // Run the network loop
                if let Err(err) = error::eval(unsafe { fdb_sys::fdb_run_network() }) {
                    #[cfg(feature = "trace")]
                    tracing::error!("FoundationDB network thread failed: {}", err);
                    eprintln!("FDB network thread error: {}", err);
                } else {
                    #[cfg(feature = "trace")]
                    tracing::info!("FoundationDB network thread exited normally");
                }

                // Mark network as stopped
                let mut state = NETWORK_STATE.lock().unwrap_or_else(|e| e.into_inner());
                state.network_running = false;
            });

            // Wait for thread to start
            {
                let (lock, cvar) = &*cond;
                let mut started = lock.lock().unwrap_or_else(|e| e.into_inner());
                while !*started {
                    started = cvar.wait(started).unwrap_or_else(|e| e.into_inner());
                }
            }

            // Store thread handle and mark as running
            let mut state = NETWORK_STATE.lock().unwrap_or_else(|e| e.into_inner());
            state.thread_handle = Some(handle);
            state.network_running = true;
            #[cfg(feature = "trace")]
            tracing::debug!("Network thread handle stored and marked as running");

            Ok(())
        })();
        *THREAD_START_RESULT
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(result);
    });

    let result = THREAD_START_RESULT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .expect("Network setup result should be set after call_once")
        .clone();
    result
}

/// Stops the FoundationDB network exactly once.
///
/// This function is thread-safe and will only call `fdb_stop_network` once per process.
/// It waits for the network thread to exit cleanly. Once called, the network cannot be restarted.
#[cfg_attr(feature = "trace", tracing::instrument(level = "info"))]
fn stop_network_once() {
    NETWORK_STOP.call_once(|| {
        #[cfg(feature = "trace")]
        tracing::info!("Stopping FoundationDB network");

        // Get thread handle
        let handle = {
            let mut state = NETWORK_STATE.lock().unwrap_or_else(|e| e.into_inner());
            let h = state.thread_handle.take();
            #[cfg(feature = "trace")]
            tracing::debug!("Got network thread handle: {}", h.is_some());
            h
        };

        // Stop the network
        if let Err(err) = error::eval(unsafe { fdb_sys::fdb_stop_network() }) {
            #[cfg(feature = "trace")]
            tracing::error!("Failed to stop FoundationDB network: {}", err);
            panic!("Failed to stop FoundationDB network: {}. This is a critical error that prevents proper resource cleanup.", err);
        }
        #[cfg(feature = "trace")]
        tracing::debug!("FoundationDB network stop signal sent");

        // Join thread if we have a handle
        if let Some(handle) = handle {
            if let Err(_) = handle.join() {
                #[cfg(feature = "trace")]
                tracing::error!("Failed to join FoundationDB network thread");
                eprintln!("Failed to join network thread");
            } else {
                #[cfg(feature = "trace")]
                tracing::info!("FoundationDB network thread stopped successfully");
            }
        }
    });
}

// High-level orchestrators

/// Ensures the FoundationDB network is fully initialized and running.
///
/// This function coordinates the complete network startup sequence in the correct order:
/// API version selection, network setup, and thread startup. It maintains a reference count
/// of active users and is safe to call from multiple threads concurrently.
/// Each call increments an internal reference counter.
#[cfg_attr(feature = "trace", tracing::instrument(level = "info"))]
pub(crate) fn ensure_network_running() -> FdbResult<()> {
    // Check if network has been stopped (can't restart)
    {
        let state = NETWORK_STATE.lock().unwrap_or_else(|e| e.into_inner());
        if state.stop_requested {
            #[cfg(feature = "trace")]
            tracing::warn!("Attempted to start network that was already stopped");
            return Err(error::FdbError::from_code(
                error_codes::NETWORK_ALREADY_STOPPED,
            ));
        }
    }

    ensure_api_version_set(fdb_sys::FDB_API_VERSION as i32)?;
    ensure_network_setup()?;
    ensure_network_thread_running()?;

    // Increment reference count
    let mut state = NETWORK_STATE.lock().unwrap_or_else(|e| e.into_inner());
    // Double-check stop wasn't requested while we were setting up
    if state.stop_requested {
        #[cfg(feature = "trace")]
        tracing::warn!("Network stop was requested during initialization");
        return Err(error::FdbError::from_code(
            error_codes::NETWORK_ALREADY_STOPPED,
        ));
    }
    let _old_count = state.ref_count;
    state.ref_count += 1;

    #[cfg(feature = "trace")]
    tracing::debug!(
        "Network reference count: {} -> {}",
        _old_count,
        state.ref_count
    );

    Ok(())
}

/// Releases a reference to the FoundationDB network.
///
/// This function decrements the internal reference counter. When the counter reaches zero,
/// it automatically stops the network thread and cleans up resources. This is called
/// automatically when Database objects are dropped.
#[cfg_attr(feature = "trace", tracing::instrument(level = "debug"))]
pub(crate) fn release_network() {
    let should_stop = {
        let mut state = NETWORK_STATE.lock().unwrap_or_else(|e| e.into_inner());

        #[cfg(feature = "trace")]
        tracing::debug!(
            "Network state: ref_count={}, stop_requested={}, network_running={}",
            state.ref_count,
            state.stop_requested,
            state.network_running
        );

        // Only decrement if ref_count > 0
        if state.ref_count > 0 {
            state.ref_count -= 1;

            #[cfg(feature = "trace")]
            tracing::debug!("Network reference count decremented to {}", state.ref_count);

            // Only stop if this was the last reference AND we haven't already requested stop
            if state.ref_count == 0 && !state.stop_requested && state.network_running {
                state.stop_requested = true;
                #[cfg(feature = "trace")]
                tracing::info!("Last network reference dropped, stopping network");
                true
            } else {
                #[cfg(feature = "trace")]
                tracing::debug!(
                    "Network not stopped (ref_count={}, stop_requested={}, network_running={})",
                    state.ref_count,
                    state.stop_requested,
                    state.network_running
                );
                false
            }
        } else {
            #[cfg(feature = "trace")]
            tracing::warn!("Network reference count already at 0, not decrementing");
            false
        }
    };

    if should_stop {
        stop_network_once();
    }
}

/// A Builder with which different versions of the Fdb C API can be initialized
///
/// The foundationDB C API can only be initialized once.
///
/// Example initialization flow documented in the README.
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
    pub fn build(self) -> FdbResult<NetworkBuilder> {
        ensure_api_version_set(self.runtime_version)?;
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
/// Example network boot flow documented in the README.
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
    /// Example manual network management flow documented in the README.
    pub fn build(self) -> FdbResult<(NetworkRunner, NetworkWait)> {
        ensure_network_setup()?;
        Ok((NetworkRunner { _private: () }, NetworkWait { _private: () }))
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
    /// You *MUST* ensure `drop` is called on the returned object before the program exits.
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
    /// Example boot usage documented in the README.
    ///
    /// # Deprecation Timeline
    ///
    /// - **v0.9.0** (Current): Deprecated in favor of automatic network management
    /// - **v0.10.0** (Planned Q2 2026): Will emit compilation warnings
    /// - **v1.0.0** (Planned Q4 2026): Will be removed completely
    ///
    /// **Migration Path**: Replace `unsafe { boot() }` with direct `Database::new(None)` calls.
    #[deprecated(
        since = "0.10.0",
        note = "Network is managed automatically by Database. Just create a Database directly."
    )]
    pub unsafe fn boot(self) -> FdbResult<NetworkAutoStop> {
        NetworkAutoStop::new()
    }
}

/// A foundationDB network event loop runner
///
/// Most of the time you should never need to use this directly and use `boot()`.
pub struct NetworkRunner {
    _private: (),
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
        // Use the unified network management system
        ensure_network_running()?;
        Ok(())
    }
}

/// A condition object that can wait for the associated `NetworkRunner` to actually run.
///
/// Most of the time you should never need to use this directly and use `boot()`.
pub struct NetworkWait {
    _private: (),
}

impl NetworkWait {
    /// Wait for the associated `NetworkRunner` to actually run.
    pub fn wait(self) -> NetworkStop {
        // With the unified system, just ensure the network is running
        // This will either start it or return immediately if already started
        let _ = ensure_network_running();
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
        stop_network_once();
        Ok(())
    }
}

/// Stop the associated `NetworkRunner` and thread if dropped
///
/// This is a compatibility shim that uses the unified network management system.
pub struct NetworkAutoStop {
    _private: (),
}

impl NetworkAutoStop {
    pub(crate) fn new() -> FdbResult<Self> {
        ensure_network_running()?;
        Ok(NetworkAutoStop { _private: () })
    }
}

impl Drop for NetworkAutoStop {
    fn drop(&mut self) {
        release_network();
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
