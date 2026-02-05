//! Types, constants, and enums for leader election simulation workload.

use foundationdb::recipes::leader_election::{CandidateInfo, ElectionConfig, LeaderState};
use foundationdb::tuple::Versionstamp;

// Use i64 instead of u8 for tuple packing (u8 is not supported)
pub(crate) const OP_REGISTER: i64 = 0;
pub(crate) const OP_HEARTBEAT: i64 = 1;
pub(crate) const OP_TRY_BECOME_LEADER: i64 = 2;
pub(crate) const OP_RESIGN: i64 = 3;

/// A single log entry - entries are read in FDB commit order (versionstamp ordering)
#[derive(Debug, Clone)]
pub(crate) struct LogEntry {
    pub versionstamp: Versionstamp,
    pub client_id: i32,
    pub op_num: u64,
    pub op_type: i64,
    pub success: bool,
    pub became_leader: bool,

    // Ballot tracking for invariant checks
    /// Ballot number after the operation completed
    pub ballot: u64,
    /// Ballot number before the operation (for detecting transitions)
    pub previous_ballot: u64,

    // Lease tracking for overlap detection
    /// Lease expiry timestamp in nanoseconds (for leadership claims)
    pub lease_expiry_nanos: i64,
    /// Timestamp when leadership claim was made (for lease validity checks)
    pub claim_timestamp_nanos: i64,
}

/// Log entries in FDB commit order (versionstamp-ordered keys give us true ordering)
pub(crate) type LogEntries = Vec<LogEntry>;

/// Operation types for logging
#[derive(Clone, Copy, Debug)]
pub(crate) enum OpType {
    Register,
    Heartbeat,
    TryBecomeLeader,
    Resign,
}

impl OpType {
    pub(crate) fn from_i64(val: i64) -> Option<Self> {
        match val {
            OP_REGISTER => Some(OpType::Register),
            OP_HEARTBEAT => Some(OpType::Heartbeat),
            OP_TRY_BECOME_LEADER => Some(OpType::TryBecomeLeader),
            OP_RESIGN => Some(OpType::Resign),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            OpType::Register => "Register",
            OpType::Heartbeat => "Heartbeat",
            OpType::TryBecomeLeader => "TryBecomeLeader",
            OpType::Resign => "Resign",
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
    pub(crate) resign_count: usize,
    pub(crate) error_count: usize,
    pub(crate) op_nums: Vec<u64>,
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
