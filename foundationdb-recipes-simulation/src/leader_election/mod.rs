//! Leader Election Simulation Workload
//!
//! Tests the leader election recipe under chaos conditions including
//! network partitions, process failures, and cluster reconfigurations.
//!
//! # Check Phase Invariants
//!
//! The check phase validates the core Active Disk Paxos invariants:
//!
//! 1. **Safety (No Overlapping Leadership)**: At most one leader at any time
//! 2. **Ballot Conservation**: Expected ballot from logs matches actual ballot in FDB

mod helpers;
mod invariants;
mod types;
mod workload;

use foundationdb::tuple::Subspace;
use foundationdb_simulation::{SingleRustWorkload, WorkloadContext};

use types::ClockSkewLevel;

pub struct LeaderElectionWorkload {
    pub(crate) context: WorkloadContext,
    pub(crate) client_id: i32,
    pub(crate) client_count: i32,

    // Configuration from TOML
    pub(crate) operation_count: usize,
    pub(crate) heartbeat_timeout_secs: u64,
    pub(crate) resign_probability: f64,

    // Subspaces
    pub(crate) election_subspace: Subspace,
    pub(crate) log_subspace: Subspace,

    // State
    pub(crate) process_id: String,
    pub(crate) op_num: u64,

    // Clock skew simulation (mimics FDB sim2)
    pub(crate) clock_skew_level: ClockSkewLevel,
    pub(crate) clock_offset_secs: f64,
    pub(crate) clock_timer_time: f64,

    // Metrics
    pub(crate) heartbeat_count: u64,
    pub(crate) leadership_attempts: u64,
    pub(crate) times_became_leader: u64,
    pub(crate) resign_count: u64,
    pub(crate) error_count: u64,
}

impl SingleRustWorkload for LeaderElectionWorkload {
    fn new(_name: String, context: WorkloadContext) -> Self {
        let client_id = context.client_id();
        let client_count = context.client_count();

        // Randomize clock skew level per client (deterministic via context.rnd())
        let clock_skew_level = match context.rnd() % 3 {
            0 => ClockSkewLevel::Light,
            1 => ClockSkewLevel::Moderate,
            _ => ClockSkewLevel::Extreme,
        };

        // Random offset per node using context.rnd() for determinism
        // Scale rnd() output (u32) to range [-max_offset, +max_offset]
        let max_offset = clock_skew_level.max_offset_secs();
        let rnd_val = context.rnd() as f64 / u32::MAX as f64; // 0.0 to 1.0
        let clock_offset_secs = (rnd_val * 2.0 - 1.0) * max_offset; // -max to +max

        // Start timer at current time + offset
        let clock_timer_time = context.now() + clock_offset_secs;

        Self {
            operation_count: context.get_option("operationCount").unwrap_or(50),
            heartbeat_timeout_secs: context.get_option("heartbeatTimeoutSecs").unwrap_or(10),
            resign_probability: context.get_option("resignProbability").unwrap_or(0.1),
            election_subspace: Subspace::all().subspace(&("leader_election",)),
            log_subspace: Subspace::all().subspace(&("le_log",)),
            process_id: format!("process_{client_id}"),
            client_id,
            client_count,
            context,
            op_num: 0,
            clock_skew_level,
            clock_offset_secs,
            clock_timer_time,
            heartbeat_count: 0,
            leadership_attempts: 0,
            times_became_leader: 0,
            resign_count: 0,
            error_count: 0,
        }
    }
}
