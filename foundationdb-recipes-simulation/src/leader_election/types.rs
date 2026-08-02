// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Commit-log types for the leader-election simulation.

use foundationdb::tuple::Versionstamp;

pub(crate) const OP_POLL: i64 = 0;
pub(crate) const OP_RESIGN: i64 = 1;
pub(crate) const OP_OBSERVE: i64 = 2;
pub(crate) const OP_STALE_WRITE: i64 = 3;

/// One simulation operation, ordered by the versionstamp in its key.
///
/// The value deliberately repeats the transaction's election-state decision.
/// This permits replay without reading recipe-private keys or interpreting a
/// local clock. `result` is leader/follower for polls, resigned/rejected for
/// resignations, and committed/aborted for ranked-register writes.
#[derive(Debug)]
pub(crate) struct LogEntry {
    pub(crate) versionstamp: Versionstamp,
    pub(crate) client_id: i32,
    pub(crate) op_num: u64,
    pub(crate) kind: i64,
    pub(crate) actor: String,
    pub(crate) prior_generation: u64,
    pub(crate) prior_owner: Option<String>,
    pub(crate) generation: u64,
    pub(crate) owner: Option<String>,
    pub(crate) requested_rank: u64,
    pub(crate) result: bool,
    pub(crate) takeover: bool,
    pub(crate) protected_write_committed: bool,
    pub(crate) observed_write_rank: u64,
    pub(crate) observed_value: Option<Vec<u8>>,
    pub(crate) payload: Vec<u8>,
}

pub(crate) struct Snapshot {
    pub(crate) generation: u64,
    pub(crate) owner: Option<String>,
    pub(crate) protected_rank: u64,
    pub(crate) protected_value: Option<Vec<u8>>,
}
