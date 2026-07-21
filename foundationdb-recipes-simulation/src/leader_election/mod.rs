//! Leader Election Simulation Workload
//!
//! Tests the leader election recipe under chaos conditions including
//! network partitions, process failures, and cluster reconfigurations.
//!
//! # Check Phase Invariants
//!
//! The check phase replays the versionstamp-ordered operation log and compares
//! it against the committed FDB state. See [`invariants`] for the full set;
//! the headline ones are dual-path validation, global ballot succession,
//! belief-level no-overlap (with preemption disabled), and a progress gate.

mod helpers;
mod invariants;
mod types;
mod workload;

use foundationdb::recipes::leader_election::LeaseObservation;
use foundationdb::tuple::Subspace;
use foundationdb_simulation::{SingleRustWorkload, WorkloadContext};

use types::{ClockSkewLevel, DEFAULT_CLOCK_JITTER_OFFSET, DEFAULT_CLOCK_JITTER_RANGE};

pub struct LeaderElectionWorkload {
    pub(crate) context: WorkloadContext,
    pub(crate) client_id: i32,
    pub(crate) client_count: i32,

    // Configuration from TOML
    pub(crate) operation_count: usize,
    pub(crate) heartbeat_timeout_secs: u64,
    pub(crate) resign_probability: f64,
    /// Whether the election config enables priority preemption. Read in setup
    /// (client 0) to build the config; the checker reads it back from the
    /// committed config to decide whether the belief-overlap invariant applies.
    pub(crate) allow_preemption: bool,
    /// Minimum successful leadership claims a run must produce (progress gate).
    pub(crate) min_leadership_claims: usize,
    /// Minimum successful heartbeats a run must produce (progress gate).
    pub(crate) min_heartbeats: usize,

    // Subspaces
    pub(crate) election_subspace: Subspace,
    pub(crate) log_subspace: Subspace,

    // State
    pub(crate) process_id: String,
    pub(crate) op_num: u64,
    /// Lease-observation state threaded through leadership claims so an
    /// unrefreshed lease is only stolen after a full observation window.
    pub(crate) lease_observation: LeaseObservation,

    // Clock skew simulation (mimics FDB sim2)
    pub(crate) clock_skew_level: ClockSkewLevel,
    pub(crate) clock_offset_secs: f64,
    pub(crate) clock_timer_time: f64,
    /// Per-client drift bound used by `local_time`; zero disables drift so the
    /// strict (zero-skew) config makes `local_time == context.now()` exactly.
    pub(crate) clock_max_ahead: f64,
    /// Global worst-case one-sided clock deviation across all clients, used as
    /// the tolerance in time-based invariants. Zero in the strict config.
    pub(crate) max_clock_skew_secs: f64,

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

        // Maximum jitter multiplier applied to the offset in `local_time`.
        let jitter_max = DEFAULT_CLOCK_JITTER_OFFSET + DEFAULT_CLOCK_JITTER_RANGE;

        // `maxClockSkewSecs` overrides the randomized tiers with a fixed
        // per-client bound. `0.0` disables skew entirely (local_time == now).
        // When unset, each client keeps its random tier, and the global
        // worst-case (Extreme, both offset and drift, times jitter) is used as
        // the invariant tolerance.
        let skew_override: Option<f64> = context.get_option("maxClockSkewSecs");
        let (max_offset, clock_max_ahead, max_clock_skew_secs) = match skew_override {
            Some(v) => (v, v, (v + v) * jitter_max),
            None => {
                let tier = clock_skew_level.max_offset_secs();
                let extreme = ClockSkewLevel::Extreme.max_offset_secs();
                (tier, tier, (extreme + extreme) * jitter_max)
            }
        };

        // Random offset per node using context.rnd() for determinism
        // Scale rnd() output (u32) to range [-max_offset, +max_offset]
        let rnd_val = context.rnd() as f64 / u32::MAX as f64; // 0.0 to 1.0
        let clock_offset_secs = (rnd_val * 2.0 - 1.0) * max_offset; // -max to +max

        // Start timer at current time + offset
        let clock_timer_time = context.now() + clock_offset_secs;

        Self {
            operation_count: context.get_option("operationCount").unwrap_or(50),
            heartbeat_timeout_secs: context.get_option("heartbeatTimeoutSecs").unwrap_or(10),
            resign_probability: context.get_option("resignProbability").unwrap_or(0.1),
            // Integer 0/1 (not a TOML bool) to avoid ambiguity in how the C API
            // stringifies booleans. Defaults to enabled (matches the recipe).
            allow_preemption: context
                .get_option::<u64>("allowPreemption")
                .map(|v| v != 0)
                .unwrap_or(true),
            min_leadership_claims: context
                .get_option::<u64>("minLeadershipClaims")
                .map(|v| v.max(1) as usize)
                .unwrap_or(1),
            min_heartbeats: context
                .get_option::<u64>("minHeartbeats")
                .map(|v| v.max(1) as usize)
                .unwrap_or(1),
            election_subspace: Subspace::all().subspace(&("leader_election",)),
            log_subspace: Subspace::all().subspace(&("le_log",)),
            process_id: format!("process_{client_id}"),
            client_id,
            client_count,
            context,
            op_num: 0,
            lease_observation: LeaseObservation::new(),
            clock_skew_level,
            clock_offset_secs,
            clock_timer_time,
            clock_max_ahead,
            max_clock_skew_secs,
            heartbeat_count: 0,
            leadership_attempts: 0,
            times_became_leader: 0,
            resign_count: 0,
            error_count: 0,
        }
    }
}
