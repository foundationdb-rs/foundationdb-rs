//! Leader Election Simulation Workload
//!
//! This module provides a simulation workload for testing the FoundationDB
//! leader election recipe under deterministic simulation.

mod invariants;
mod operations;
mod time;
mod types;
mod workload;

use foundationdb::tuple::Subspace;
use foundationdb_simulation::{SingleRustWorkload, WorkloadContext};
use std::collections::BTreeMap;
use std::time::Duration;

use types::{ProcessState, DEFAULT_TIMEOUT_SECS};

/// Leader Election Simulation Workload
///
/// Tests the leader election algorithm under FDB's deterministic simulation
/// by exercising various operations with time anomalies and verifying invariants.
pub struct LeaderElectionWorkload {
    pub(crate) context: WorkloadContext,
    pub(crate) client_id: i32,
    pub(crate) subspace: Subspace,

    // Configuration
    pub(crate) test_duration: f64,
    pub(crate) initial_processes: usize,
    pub(crate) heartbeat_timeout: Duration,

    // Per-client clock skew (deterministic based on client_id)
    pub(crate) clock_skew: Duration,

    // Process management - use BTreeMap for determinism
    pub(crate) processes: BTreeMap<String, ProcessState>,
    pub(crate) next_process_num: u32,

    // Operation counter for log keys
    pub(crate) op_num: u64,

    // Metrics
    pub(crate) total_operations: u64,
    pub(crate) successful_operations: u64,
    pub(crate) failed_operations: u64,
    pub(crate) leadership_claims: u64,
    pub(crate) registrations: u64,
    pub(crate) heartbeats: u64,
    pub(crate) time_anomalies_injected: u64,

    // Election state tracking
    pub(crate) election_enabled: bool,
}

impl LeaderElectionWorkload {
    /// Generate a deterministic random number in range [0, max)
    pub(crate) fn random_range(&self, max: u32) -> u32 {
        self.context.rnd() % max
    }

    /// Generate a new process UUID
    pub(crate) fn generate_process_uuid(&mut self) -> String {
        let uuid = format!("client{}-proc{}", self.client_id, self.next_process_num);
        self.next_process_num += 1;
        uuid
    }

    /// Get log key for this client
    pub(crate) fn log_key(&self, op_num: u64) -> Vec<u8> {
        self.subspace
            .subspace(&("log",))
            .pack(&(self.client_id, op_num))
    }

    /// Pack a log entry
    pub(crate) fn pack_log_entry(
        &self,
        op_type: i64,
        uuid: &str,
        timestamp: Duration,
        extra: &[u8],
    ) -> Vec<u8> {
        foundationdb::tuple::pack(&(op_type, uuid, timestamp.as_nanos() as i64, extra.to_vec()))
    }
}

impl SingleRustWorkload for LeaderElectionWorkload {
    fn new(_name: String, context: WorkloadContext) -> Self {
        let client_id = context.client_id();

        // Deterministic clock skew based on client_id
        let clock_skew = Duration::from_millis((client_id as u64 * 100) % 1000);

        // Get configuration from test file
        let test_duration: f64 = context.get_option("testDuration").unwrap_or(60.0);
        let initial_processes: usize = context.get_option("initialProcesses").unwrap_or(3);
        let heartbeat_timeout_secs: u64 = context
            .get_option("heartbeatTimeout")
            .unwrap_or(DEFAULT_TIMEOUT_SECS);

        Self {
            subspace: Subspace::from_bytes(b"leader_election"),
            client_id,
            test_duration,
            initial_processes,
            heartbeat_timeout: Duration::from_secs(heartbeat_timeout_secs),
            clock_skew,
            processes: BTreeMap::new(),
            next_process_num: 0,
            op_num: 0,
            total_operations: 0,
            successful_operations: 0,
            failed_operations: 0,
            leadership_claims: 0,
            registrations: 0,
            heartbeats: 0,
            time_anomalies_injected: 0,
            election_enabled: true,
            context,
        }
    }
}
