// Copyright 2018 foundationdb-rs developers, https://github.com/Clikengo/foundationdb-rs/graphs/contributors
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.
#![doc = include_str!("../README.md")]

#[macro_use]
extern crate static_assertions;

pub mod api;
#[cfg(any(feature = "fdb-5_1", feature = "fdb-5_2", feature = "fdb-6_0"))]
pub mod cluster;
mod database;
pub mod directory;
mod error;
#[cfg(any(feature = "fdb-7_0", feature = "fdb-7_1", feature = "fdb-7_3"))]
#[deny(missing_docs)]
pub mod fdb_keys;
pub mod future;
mod keyselector;
#[cfg(any(feature = "fdb-7_1", feature = "fdb-7_3"))]
#[deny(missing_docs)]
pub mod mapped_key_values;
/// Generated configuration types for use with the various `set_option` functions
#[allow(clippy::all)]
pub mod options;
#[cfg(any(
    feature = "fdb-7_1",
    feature = "fdb-7_3",
    feature = "tenant-experimental"
))]
pub mod tenant;
mod transaction;
pub mod tuple;

use crate::api::spawn_network_thread_if_needed;
#[cfg(any(feature = "fdb-5_1", feature = "fdb-5_2", feature = "fdb-6_0"))]
pub use crate::cluster::Cluster;

pub use crate::database::*;
pub use crate::error::FdbBindingError;
pub use crate::error::FdbError;
pub use crate::error::FdbResult;
pub use crate::keyselector::*;
pub use crate::transaction::*;

/// Initialize the FoundationDB Client API with the embedded API version and boot the associated
/// network thread.
///
/// Users must call `stop_network` at the end of their program.
///
/// # Examples
///
/// ```rust
/// // `boot` is calling set_api_version(current_api_version) for you
/// foundationdb::boot().expect("could not boot FDB client API");
/// // do some interesting things with the API...
/// foundationdb::stop_network().expect("could not stop network");
/// ```
///
/// An alternative way to start fdb is to do this
///
/// ```rust
/// #[tokio::main]
/// async fn main() {
///     foundationdb::api::set_api_version(730).expect("could not set api version");
///     // do some interesting things with the API...
///     foundationdb::api::stop_network().expect("could not stop network thread");
/// }
/// ```
pub fn boot() -> Result<NetworkAutoStop, FdbError> {
    api::FdbApiBuilder::default().build()?.boot()
}

pub fn shutdown() -> Result<(), FdbError> {
    api::stop_network()
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
