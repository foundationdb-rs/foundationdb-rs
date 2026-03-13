// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Key management for the ranked register
//!
//! Single key stores the entire register state (ranks + value).
//!
//! # Key Schema
//!
//! ```text
//! <subspace>/state  -> (max_read_rank: u64, max_write_rank: u64, has_value: bool, value: Bytes)
//! ```

use crate::tuple::Subspace;

/// Key prefix for register state
const STATE_PREFIX: &str = "state";

/// Generate the key for the register state
///
/// Returns the single key where all register state is stored.
///
/// # Key Structure
/// `<subspace>/state`
pub fn state_key(subspace: &Subspace) -> Vec<u8> {
    subspace.pack(&(STATE_PREFIX,))
}
