// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Key management for the ranked register
//!
//! Each [`RankedRegister`](super::RankedRegister) stores its entire state at
//! one key in its own [`Subspace`].
//!
//! # Key Schema
//!
//! ```text
//! <subspace>/state  -> (max_read_rank: u64, max_write_rank: u64, has_value: bool, value: Bytes)
//! ```
//!
//! The full tuple, including both ranks and the payload, is one FoundationDB
//! value. Its encoded size is capped at 95,000 bytes, below FoundationDB's
//! 100,000-byte value limit. Payload capacity is smaller and data-dependent
//! because tuple encoding escapes bytes. Store a small reference or manifest
//! instead when the protected data is large and immutable.
//!
//! Every ranked read or write for one subspace contends on this single key, so
//! those updates serialize through FoundationDB conflicts. Model a keyed
//! collection with one child subspace per logical key.

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
