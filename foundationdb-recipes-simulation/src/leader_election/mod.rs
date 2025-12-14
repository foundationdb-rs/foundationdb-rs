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

pub struct LeaderElectionWorkload {
    pub(crate) context: WorkloadContext,
    pub(crate) client_id: i32,
    pub(crate) client_count: i32,

    // Configuration from TOML
    pub(crate) operation_count: usize,
    pub(crate) heartbeat_timeout_secs: u64,

    // Subspaces
    pub(crate) election_subspace: Subspace,
    pub(crate) log_subspace: Subspace,

    // State
    pub(crate) process_id: String,
    pub(crate) op_num: u64,
    pub(crate) versionstamp: Option<[u8; 12]>,

    // Metrics
    pub(crate) heartbeat_count: u64,
    pub(crate) leadership_attempts: u64,
    pub(crate) times_became_leader: u64,
    pub(crate) error_count: u64,
}

impl SingleRustWorkload for LeaderElectionWorkload {
    fn new(_name: String, context: WorkloadContext) -> Self {
        let client_id = context.client_id();
        let client_count = context.client_count();

        Self {
            operation_count: context.get_option("operationCount").unwrap_or(50),
            heartbeat_timeout_secs: context.get_option("heartbeatTimeoutSecs").unwrap_or(10),
            election_subspace: Subspace::all().subspace(&("leader_election",)),
            log_subspace: Subspace::all().subspace(&("le_log",)),
            process_id: format!("process_{client_id}"),
            client_id,
            client_count,
            context,
            op_num: 0,
            versionstamp: None,
            heartbeat_count: 0,
            leadership_attempts: 0,
            times_became_leader: 0,
            error_count: 0,
        }
    }
}
