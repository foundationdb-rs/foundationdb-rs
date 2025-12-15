//! Types, constants, and enums for leader election simulation workload.

use std::collections::BTreeMap;
use std::time::Duration;

use foundationdb::recipes::leader_election::{CandidateInfo, ElectionConfig, LeaderState};

// Use i64 instead of u8 for tuple packing (u8 is not supported)
pub(crate) const OP_REGISTER: i64 = 0;
pub(crate) const OP_HEARTBEAT: i64 = 1;
pub(crate) const OP_TRY_BECOME_LEADER: i64 = 2;

/// Log entry map: (timestamp_millis, client_id, op_num) -> (op_type, success, timestamp, became_leader)
pub(crate) type LogEntries = BTreeMap<(i64, i32, u64), (i64, bool, f64, bool)>;

/// Operation types for logging
#[derive(Clone, Copy, Debug)]
pub(crate) enum OpType {
    Register,
    Heartbeat,
    TryBecomeLeader,
}

impl OpType {
    pub(crate) fn as_i64(self) -> i64 {
        match self {
            OpType::Register => OP_REGISTER,
            OpType::Heartbeat => OP_HEARTBEAT,
            OpType::TryBecomeLeader => OP_TRY_BECOME_LEADER,
        }
    }

    pub(crate) fn from_i64(val: i64) -> Option<Self> {
        match val {
            OP_REGISTER => Some(OpType::Register),
            OP_HEARTBEAT => Some(OpType::Heartbeat),
            OP_TRY_BECOME_LEADER => Some(OpType::TryBecomeLeader),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            OpType::Register => "Register",
            OpType::Heartbeat => "Heartbeat",
            OpType::TryBecomeLeader => "TryBecomeLeader",
        }
    }
}

/// Snapshot of database state for invariant checking
pub(crate) struct DatabaseSnapshot {
    pub(crate) leader_state: Option<LeaderState>,
    pub(crate) candidates: Vec<CandidateInfo>,
    pub(crate) config: Option<ElectionConfig>,
}

/// Result of running all invariant checks
pub(crate) struct CheckResult {
    pub(crate) passed: usize,
    pub(crate) failed: usize,
    pub(crate) results: Vec<(&'static str, bool, String)>,
}

/// Per-client statistics extracted from log entries
#[derive(Default, Debug)]
pub(crate) struct ClientStats {
    pub(crate) register_count: usize,
    pub(crate) heartbeat_count: usize,
    pub(crate) leadership_attempt_count: usize,
    pub(crate) leadership_success_count: usize,
    pub(crate) error_count: usize,
    pub(crate) first_timestamp: Option<f64>,
    pub(crate) last_timestamp: Option<f64>,
    pub(crate) op_nums: Vec<u64>,
}

/// Get lease duration from snapshot or use default
pub(crate) fn get_lease_duration(snapshot: &DatabaseSnapshot, default_secs: u64) -> Duration {
    snapshot
        .config
        .as_ref()
        .map(|c| c.lease_duration)
        .unwrap_or(Duration::from_secs(default_secs))
}

// ============================================================================
// CLOCK SKEW SIMULATION (mimics FDB's sim2.actor.cpp)
// ============================================================================

/// Clock skew defaults (mimicking FDB's sim2)
pub const DEFAULT_CLOCK_JITTER_RANGE: f64 = 0.2; // ±10% like DELAY_JITTER_RANGE
pub const DEFAULT_CLOCK_JITTER_OFFSET: f64 = 0.9; // Like DELAY_JITTER_OFFSET

/// Clock skew levels for simulation
#[derive(Clone, Copy, Debug)]
pub(crate) enum ClockSkewLevel {
    /// Light: ±100ms (like FDB's timer() vs now())
    Light,
    /// Moderate: ±500ms (cloud NTP worst case)
    Moderate,
    /// Extreme: ±1s (stress test, will cause election churn)
    Extreme,
}

impl ClockSkewLevel {
    /// Maximum clock offset in seconds for this skew level
    pub fn max_offset_secs(&self) -> f64 {
        match self {
            ClockSkewLevel::Light => 0.1,
            ClockSkewLevel::Moderate => 0.5,
            ClockSkewLevel::Extreme => 1.0,
        }
    }
}
