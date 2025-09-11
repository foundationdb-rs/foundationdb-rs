// Copyright 2018 foundationdb-rs developers, https://github.com/Clikengo/foundationdb-rs/graphs/contributors
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.
#![doc = include_str!("../README.md")]

use foundationdb_sys::if_cfg_api_versions;

#[macro_use]
extern crate static_assertions;

pub mod api;
if_cfg_api_versions! {min = 510, max = 600 =>
    pub mod cluster;
}
mod database;
pub mod directory;
mod error;
if_cfg_api_versions! {min = 700 =>
    #[deny(missing_docs)]
    pub mod fdb_keys;
}
pub mod future;
mod keyselector;
if_cfg_api_versions! {min = 710 =>
    #[deny(missing_docs)]
    pub mod mapped_key_values;
}

pub mod metrics;

/// Generated configuration types for use with the various `set_option` functions
#[allow(clippy::all)]
pub mod options;
if_cfg_api_versions! {min = 710 =>
    #[cfg(feature = "tenant-experimental")]
    pub mod tenant;
}

// Re-export metrics types for convenience
pub use crate::metrics::TransactionMetrics;
pub mod timekeeper;
mod transaction;
mod tuple_ext;

pub mod tuple {
    pub use crate::tuple_ext::*;
    pub use foundationdb_tuple::*;
}

if_cfg_api_versions! {min = 510, max = 600 =>
    pub use crate::cluster::Cluster;
}

pub use crate::database::*;
pub use crate::error::FdbBindingError;
pub use crate::error::{FdbError, FdbResult};
pub use crate::keyselector::*;
pub use crate::transaction::*;

/// Initialize the FoundationDB Client API, this can only be called once per process.
///
/// # Returns
///
/// A `NetworkAutoStop` handle which must be dropped before the program exits.
///
/// # Safety
///
/// You *MUST* ensure drop is called on the returned object before the program exits.
/// This is not required if the program is aborted.
///
/// This method used to be safe in version `0.4`. But because `drop` on the returned object
/// might not be called before the program exits, it was found unsafe.
///
/// # Examples
///
/// ```rust
/// let network = unsafe { foundationdb::boot() };
/// // do some interesting things with the API...
/// drop(network);
/// ```
///
/// ```rust
/// #[tokio::main]
/// async fn main() {
///     let network = unsafe { foundationdb::boot() };
///     // do some interesting things with the API...
///     drop(network);
/// }
/// ```
#[deprecated(
    note = "Network is managed automatically by Database. Just use Database::new() or Database::new_compat()"
)]
pub unsafe fn boot() -> FdbResult<api::NetworkAutoStop> {
    api::NetworkAutoStop::new()
}

/// Returns the default Fdb cluster configuration file path
#[cfg(target_os = "linux")]
pub fn default_config_path() -> &'static str {
    "/etc/foundationdb/fdb.cluster"
}

/// Returns the default Fdb cluster configuration file path
#[cfg(target_os = "macos")]
pub fn default_config_path() -> &'static str {
    "/usr/local/etc/foundationdb/fdb.cluster"
}

/// Returns the default Fdb cluster configuration file path
#[cfg(target_os = "windows")]
pub fn default_config_path() -> &'static str {
    "C:/ProgramData/foundationdb/fdb.cluster"
}

/// slice::from_raw_parts assumes the pointer to be aligned and non-null.
/// Since Rust nightly (mid-February 2024), it is enforced with debug asserts,
/// but the FDBServer can return a null pointer if the slice is empty:
/// ptr: 0x0, len: 0
/// To avoid the assert, an empty slice is directly returned in that situation
fn from_raw_fdb_slice<T, U: Into<usize>>(ptr: *const T, len: U) -> &'static [T] {
    if ptr.is_null() {
        return &[];
    }
    unsafe { std::slice::from_raw_parts(ptr, len.into()) }
}
