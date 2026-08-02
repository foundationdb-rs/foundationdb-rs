// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Key management for leader election.
//!
//! A single key holds the durable coordination state. It deliberately contains
//! no time value: takeover timing is a local caller concern.

use crate::tuple::Subspace;

const STATE_PREFIX: &str = "state";

/// Returns the single durable election-state key.
pub(crate) fn state_key(subspace: &Subspace) -> Vec<u8> {
    subspace.pack(&(STATE_PREFIX,))
}
