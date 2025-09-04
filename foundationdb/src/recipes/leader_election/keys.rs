// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Key management utilities for leader election
//!
//! This module defines the key structure and namespace organization for
//! storing leader election data in FoundationDB. All keys are scoped
//! within a user-provided subspace to allow multiple elections to coexist.

use crate::tuple::Subspace;

/// Key prefixes for different data types
///
/// These prefixes organize the election data into logical groups within the subspace
pub const CONFIG_PREFIX: &str = "config";
pub const PROCESSES_PREFIX: &str = "processes";
pub const LEADER_STATE_PREFIX: &str = "leader_state";

/// Generate the key for global configuration
///
/// Returns the key where election configuration is stored.
///
/// # Key Structure
/// `<subspace>/config`
pub fn config_key(subspace: &Subspace) -> Vec<u8> {
    subspace.pack(&(CONFIG_PREFIX,))
}

/// Generate the key for a specific process
///
/// Returns the key for storing a process's versionstamp.
///
/// # Key Structure
/// `<subspace>/processes/<process_uuid>`
///
/// # Arguments
/// * `process_uuid` - Unique identifier for the process
pub fn process_key(subspace: &Subspace, process_uuid: &str) -> Vec<u8> {
    subspace.pack(&(PROCESSES_PREFIX, process_uuid))
}

/// Generate the key range for all processes
///
/// Returns the start and end keys for scanning all registered processes.
///
/// # Key Structure
/// Range: `<subspace>/processes/` to `<subspace>/processes/\xff`
///
/// # Usage
/// Used for finding all alive processes during leader election
pub fn processes_range(subspace: &Subspace) -> (Vec<u8>, Vec<u8>) {
    let start = subspace.subspace(&(PROCESSES_PREFIX,));
    start.range()
}

/// Generate the key for leader state
///
/// Returns the key where current leader information is stored.
///
/// # Key Structure
/// `<subspace>/leader_state`
///
/// # Contents
/// Stores the current leader's versionstamp and lease information
pub fn leader_state_key(subspace: &Subspace) -> Vec<u8> {
    subspace.pack(&(LEADER_STATE_PREFIX,))
}
