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
//!
//! # Key Schema
//!
//! ```text
//! <subspace>/config           - Election configuration
//! <subspace>/leader           - Current leader state (single key, O(1) access)
//! <subspace>/candidates/<id>  - Per-candidate registration and heartbeat
//! ```

use crate::tuple::Subspace;

/// Key prefixes for different data types
pub const CONFIG_PREFIX: &str = "config";
pub const LEADER_PREFIX: &str = "leader";
pub const CANDIDATES_PREFIX: &str = "candidates";

/// Generate the key for global configuration
///
/// Returns the key where election configuration is stored.
///
/// # Key Structure
/// `<subspace>/config`
pub fn config_key(subspace: &Subspace) -> Vec<u8> {
    subspace.pack(&(CONFIG_PREFIX,))
}

/// Generate the key for leader state
///
/// Returns the key where current leader information is stored.
/// This is the core "active disk" key for O(1) leadership operations.
///
/// # Key Structure
/// `<subspace>/leader`
///
/// # Contents
/// Stores the current leader's ballot, ID, priority, lease expiry, and versionstamp
pub fn leader_key(subspace: &Subspace) -> Vec<u8> {
    subspace.pack(&(LEADER_PREFIX,))
}

/// Generate the key for a specific candidate
///
/// Returns the key for storing a candidate's registration and heartbeat info.
///
/// # Key Structure
/// `<subspace>/candidates/<process_id>`
///
/// # Arguments
/// * `process_id` - Unique identifier for the candidate process
pub fn candidate_key(subspace: &Subspace, process_id: &str) -> Vec<u8> {
    subspace.pack(&(CANDIDATES_PREFIX, process_id))
}

/// Generate the key range for all candidates
///
/// Returns the start and end keys for scanning all registered candidates.
///
/// # Key Structure
/// Range: `<subspace>/candidates/` to `<subspace>/candidates/\xff`
///
/// # Usage
/// Used for listing candidates, evicting dead candidates, or monitoring.
/// Note: Leadership determination is O(1) and doesn't require scanning.
pub fn candidates_range(subspace: &Subspace) -> (Vec<u8>, Vec<u8>) {
    let start = subspace.subspace(&(CANDIDATES_PREFIX,));
    start.range()
}
