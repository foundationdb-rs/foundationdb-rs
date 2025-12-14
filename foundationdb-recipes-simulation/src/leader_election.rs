//! Leader Election Simulation Workload
//!
//! Tests the leader election recipe under chaos conditions including
//! network partitions, process failures, and cluster reconfigurations.
//!
//! # Check Phase Invariants
//!
//! The check phase validates the following invariants inspired by FDB's AtomicOps workload:
//!
//! 1. **Log Entry Completeness**: All clients logged their operations with sequential op_nums
//! 2. **Timestamp Monotonicity**: Each client's timestamps are monotonically increasing
//! 3. **Safety (No Overlapping Leadership)**: At most one leader at any time
//! 4. **Ballot Conservation**: Expected ballot from logs matches actual ballot in FDB
//! 5. **Candidate Registration Consistency**: All heartbeating clients are registered
//! 6. **Leader Is Registered Candidate**: Current leader exists in candidate list
//! 7. **Operation Sequencing**: Registrations happen before heartbeats
//! 8. **Error Rate Bounds**: Error rate is within acceptable threshold

use std::collections::BTreeMap;
use std::time::Duration;

use foundationdb::{
    options::TransactionOption,
    recipes::leader_election::{CandidateInfo, ElectionConfig, LeaderElection, LeaderState},
    tuple::{pack, unpack, Subspace},
    FdbBindingError, RangeOption,
};
use foundationdb_simulation::{
    details, Metric, Metrics, RustWorkload, Severity, SimDatabase, SingleRustWorkload,
    WorkloadContext,
};
use futures::TryStreamExt;

// Use i64 instead of u8 for tuple packing (u8 is not supported)
const OP_REGISTER: i64 = 0;
const OP_HEARTBEAT: i64 = 1;
const OP_TRY_BECOME_LEADER: i64 = 2;

/// Maximum acceptable error rate (10%)
const MAX_ERROR_RATE: f64 = 0.10;

/// Log entry map: (timestamp_millis, client_id, op_num) -> (op_type, success, timestamp, became_leader)
type LogEntries = BTreeMap<(i64, i32, u64), (i64, bool, f64, bool)>;

/// Operation types for logging
#[derive(Clone, Copy, Debug)]
enum OpType {
    Register,
    Heartbeat,
    TryBecomeLeader,
}

impl OpType {
    fn as_i64(self) -> i64 {
        match self {
            OpType::Register => OP_REGISTER,
            OpType::Heartbeat => OP_HEARTBEAT,
            OpType::TryBecomeLeader => OP_TRY_BECOME_LEADER,
        }
    }

    fn from_i64(val: i64) -> Option<Self> {
        match val {
            OP_REGISTER => Some(OpType::Register),
            OP_HEARTBEAT => Some(OpType::Heartbeat),
            OP_TRY_BECOME_LEADER => Some(OpType::TryBecomeLeader),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            OpType::Register => "Register",
            OpType::Heartbeat => "Heartbeat",
            OpType::TryBecomeLeader => "TryBecomeLeader",
        }
    }
}

/// Snapshot of database state for invariant checking
struct DatabaseSnapshot {
    leader_state: Option<LeaderState>,
    candidates: Vec<CandidateInfo>,
    config: Option<ElectionConfig>,
}

/// Result of running all invariant checks
struct CheckResult {
    passed: usize,
    failed: usize,
    results: Vec<(&'static str, bool, String)>,
}

/// Per-client statistics extracted from log entries
#[derive(Default, Debug)]
struct ClientStats {
    register_count: usize,
    heartbeat_count: usize,
    leadership_attempt_count: usize,
    leadership_success_count: usize,
    error_count: usize,
    first_timestamp: Option<f64>,
    last_timestamp: Option<f64>,
    op_nums: Vec<u64>,
}

pub struct LeaderElectionWorkload {
    context: WorkloadContext,
    client_id: i32,
    client_count: i32,

    // Configuration from TOML
    operation_count: usize,
    heartbeat_timeout_secs: u64,

    // Subspaces
    election_subspace: Subspace,
    log_subspace: Subspace,

    // State
    process_id: String,
    op_num: u64,
    versionstamp: Option<[u8; 12]>,

    // Metrics
    heartbeat_count: u64,
    leadership_attempts: u64,
    times_became_leader: u64,
    error_count: u64,
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

impl LeaderElectionWorkload {
    /// Log an operation to the log subspace for verification
    async fn log_operation(
        &mut self,
        db: &SimDatabase,
        op_type: OpType,
        success: bool,
        became_leader: bool,
    ) {
        let log_key = self.log_subspace.pack(&(self.client_id, self.op_num));
        let timestamp_secs = self.context.now();

        // Pack log entry as tuple: (op_type, success, timestamp, became_leader)
        // Use i64 for op_type since u8 doesn't implement TuplePack
        let log_value = pack(&(op_type.as_i64(), success, timestamp_secs, became_leader));

        let result = db
            .run(|trx, _maybe_committed| {
                let log_key = log_key.clone();
                let log_value = log_value.clone();
                async move {
                    trx.set_option(TransactionOption::AutomaticIdempotency)?;
                    trx.set(&log_key, &log_value);
                    Ok(())
                }
            })
            .await;

        if result.is_err() {
            self.context.trace(
                Severity::Warn,
                "LogOperationFailed",
                details![
                    "Client" => self.client_id,
                    "OpNum" => self.op_num
                ],
            );
        }

        self.op_num += 1;
    }

    // ========================================================================
    // TRACE HELPERS
    // ========================================================================

    fn trace_check_start(&self) {
        self.context.trace(
            Severity::Info,
            "CheckPhaseStart",
            details![
                "Layer" => "Rust",
                "Client" => self.client_id,
                "ClientCount" => self.client_count,
                "OperationCount" => self.operation_count
            ],
        );
    }

    fn trace_invariant_pass(&self, name: &str, details_str: &str) {
        self.context.trace(
            Severity::Info,
            "InvariantPassed",
            details![
                "Layer" => "Rust",
                "Invariant" => name,
                "Details" => details_str
            ],
        );
    }

    fn trace_invariant_fail(&self, name: &str, expected: &str, actual: &str) {
        self.context.trace(
            Severity::Error,
            "InvariantFailed",
            details![
                "Layer" => "Rust",
                "Invariant" => name,
                "Expected" => expected,
                "Actual" => actual
            ],
        );
    }

    fn trace_check_summary(&self, passed: usize, failed: usize) {
        let severity = if failed > 0 {
            Severity::Error
        } else {
            Severity::Info
        };
        self.context.trace(
            severity,
            "CheckPhaseSummary",
            details![
                "Layer" => "Rust",
                "InvariantsPassed" => passed,
                "InvariantsFailed" => failed,
                "TotalInvariants" => passed + failed,
                "Success" => failed == 0
            ],
        );
    }

    // ========================================================================
    // DATABASE STATE DUMP HELPERS (AtomicOps-style)
    // ========================================================================

    /// Dump all log entries with running statistics (like AtomicOps dumpLogKV)
    fn dump_log_entries(&self, entries: &LogEntries) {
        let mut running_leadership_count: u64 = 0;
        let mut per_client_ops: BTreeMap<i32, usize> = BTreeMap::new();

        for ((ts_millis, client_id, op_num), (op_type, success, timestamp, became_leader)) in
            entries
        {
            if *became_leader {
                running_leadership_count += 1;
            }
            *per_client_ops.entry(*client_id).or_default() += 1;

            let op_name = OpType::from_i64(*op_type)
                .map(|o| o.as_str())
                .unwrap_or("Unknown");

            self.context.trace(
                Severity::Debug,
                "LogEntryDump",
                details![
                    "Layer" => "Rust",
                    "TimestampMillis" => ts_millis,
                    "ClientId" => client_id,
                    "OpNum" => op_num,
                    "OpType" => op_name,
                    "Success" => success,
                    "Timestamp" => timestamp,
                    "BecameLeader" => became_leader,
                    "RunningLeadershipCount" => running_leadership_count
                ],
            );
        }

        // Summary per client
        for (client_id, count) in &per_client_ops {
            self.context.trace(
                Severity::Debug,
                "LogEntryClientSummary",
                details![
                    "Layer" => "Rust",
                    "ClientId" => client_id,
                    "TotalOps" => count
                ],
            );
        }
    }

    /// Dump current leader state from FDB (like AtomicOps dumpOpsKV)
    fn dump_leader_state(&self, snapshot: &DatabaseSnapshot) {
        match &snapshot.leader_state {
            Some(leader) => {
                self.context.trace(
                    Severity::Info,
                    "LeaderStateDump",
                    details![
                        "Layer" => "Rust",
                        "HasLeader" => true,
                        "LeaderId" => leader.leader_id.clone(),
                        "Ballot" => leader.ballot,
                        "Priority" => leader.priority,
                        "LeaseExpiryNanos" => leader.lease_expiry_nanos,
                        "Versionstamp" => format!("{:?}", leader.versionstamp)
                    ],
                );
            }
            None => {
                self.context.trace(
                    Severity::Info,
                    "LeaderStateDump",
                    details![
                        "Layer" => "Rust",
                        "HasLeader" => false
                    ],
                );
            }
        }
    }

    /// Dump all candidates from FDB (like AtomicOps dumpDebugKV)
    fn dump_candidates(&self, snapshot: &DatabaseSnapshot) {
        self.context.trace(
            Severity::Info,
            "CandidatesDumpStart",
            details![
                "Layer" => "Rust",
                "CandidateCount" => snapshot.candidates.len()
            ],
        );

        for candidate in &snapshot.candidates {
            self.context.trace(
                Severity::Debug,
                "CandidateDump",
                details![
                    "Layer" => "Rust",
                    "ProcessId" => candidate.process_id.clone(),
                    "Priority" => candidate.priority,
                    "LastHeartbeatNanos" => candidate.last_heartbeat_nanos,
                    "Versionstamp" => format!("{:?}", candidate.versionstamp)
                ],
            );
        }
    }

    /// Dump election config
    fn dump_config(&self, snapshot: &DatabaseSnapshot) {
        match &snapshot.config {
            Some(config) => {
                self.context.trace(
                    Severity::Info,
                    "ConfigDump",
                    details![
                        "Layer" => "Rust",
                        "HasConfig" => true,
                        "LeaseDurationSecs" => config.lease_duration.as_secs_f64(),
                        "HeartbeatIntervalSecs" => config.heartbeat_interval.as_secs_f64(),
                        "CandidateTimeoutSecs" => config.candidate_timeout.as_secs_f64(),
                        "ElectionEnabled" => config.election_enabled,
                        "AllowPreemption" => config.allow_preemption
                    ],
                );
            }
            None => {
                self.context.trace(
                    Severity::Warn,
                    "ConfigDump",
                    details![
                        "Layer" => "Rust",
                        "HasConfig" => false
                    ],
                );
            }
        }
    }

    // ========================================================================
    // STATISTICS EXTRACTION
    // ========================================================================

    /// Extract per-client statistics from log entries
    fn extract_client_stats(&self, entries: &LogEntries) -> BTreeMap<i32, ClientStats> {
        let mut stats: BTreeMap<i32, ClientStats> = BTreeMap::new();

        for ((_, client_id, op_num), (op_type, success, timestamp, became_leader)) in entries {
            let client_stats = stats.entry(*client_id).or_default();
            client_stats.op_nums.push(*op_num);

            // Update timestamps
            if client_stats.first_timestamp.is_none()
                || *timestamp < client_stats.first_timestamp.unwrap()
            {
                client_stats.first_timestamp = Some(*timestamp);
            }
            if client_stats.last_timestamp.is_none()
                || *timestamp > client_stats.last_timestamp.unwrap()
            {
                client_stats.last_timestamp = Some(*timestamp);
            }

            // Count by operation type
            match OpType::from_i64(*op_type) {
                Some(OpType::Register) => {
                    client_stats.register_count += 1;
                    if !*success {
                        client_stats.error_count += 1;
                    }
                }
                Some(OpType::Heartbeat) => {
                    client_stats.heartbeat_count += 1;
                    if !*success {
                        client_stats.error_count += 1;
                    }
                }
                Some(OpType::TryBecomeLeader) => {
                    client_stats.leadership_attempt_count += 1;
                    if *became_leader {
                        client_stats.leadership_success_count += 1;
                    }
                    if !*success {
                        client_stats.error_count += 1;
                    }
                }
                None => {}
            }
        }

        stats
    }

    /// Log statistics summary
    fn log_statistics(&self, entries: &LogEntries, stats: &BTreeMap<i32, ClientStats>) {
        let total_entries = entries.len();
        let total_leadership_claims: usize =
            stats.values().map(|s| s.leadership_success_count).sum();
        let total_errors: usize = stats.values().map(|s| s.error_count).sum();

        self.context.trace(
            Severity::Info,
            "LogStatistics",
            details![
                "Layer" => "Rust",
                "TotalLogEntries" => total_entries,
                "TotalClients" => stats.len(),
                "TotalLeadershipClaims" => total_leadership_claims,
                "TotalErrors" => total_errors
            ],
        );

        for (client_id, client_stats) in stats {
            self.context.trace(
                Severity::Info,
                "ClientStatistics",
                details![
                    "Layer" => "Rust",
                    "ClientId" => client_id,
                    "RegisterCount" => client_stats.register_count,
                    "HeartbeatCount" => client_stats.heartbeat_count,
                    "LeadershipAttempts" => client_stats.leadership_attempt_count,
                    "LeadershipSuccesses" => client_stats.leadership_success_count,
                    "Errors" => client_stats.error_count,
                    "OpCount" => client_stats.op_nums.len()
                ],
            );
        }
    }

    // ========================================================================
    // INVARIANT CHECKS
    // ========================================================================

    /// Invariant 1: Log Entry Completeness
    /// - Every client should have logged operations
    /// - Op numbers should be sequential per client (0, 1, 2, ...)
    fn verify_log_completeness(
        &self,
        _entries: &LogEntries,
        stats: &BTreeMap<i32, ClientStats>,
    ) -> (bool, String) {
        let mut issues = Vec::new();

        // Check all clients logged something
        for client_id in 0..self.client_count {
            if !stats.contains_key(&client_id) {
                issues.push(format!("Client {} has no log entries", client_id));
            }
        }

        // Check op_nums are sequential per client
        for (client_id, client_stats) in stats {
            let mut sorted_ops = client_stats.op_nums.clone();
            sorted_ops.sort();

            for (i, op_num) in sorted_ops.iter().enumerate() {
                if *op_num != i as u64 {
                    issues.push(format!(
                        "Client {}: op_num gap at index {}, expected {}, got {}",
                        client_id, i, i, op_num
                    ));
                    break;
                }
            }
        }

        if issues.is_empty() {
            (
                true,
                format!(
                    "All {} clients logged sequential operations",
                    stats.len()
                ),
            )
        } else {
            (false, issues.join("; "))
        }
    }

    /// Invariant 2: Timestamp Monotonicity Per Client
    /// - Each client's log entries should have monotonically increasing timestamps
    fn verify_timestamp_monotonicity(&self, entries: &LogEntries) -> (bool, String) {
        // Group entries by client
        let mut client_entries: BTreeMap<i32, Vec<(u64, f64)>> = BTreeMap::new();

        for ((_, client_id, op_num), (_, _, timestamp, _)) in entries {
            client_entries
                .entry(*client_id)
                .or_default()
                .push((*op_num, *timestamp));
        }

        let mut issues = Vec::new();

        for (client_id, mut ops) in client_entries {
            // Sort by op_num to get chronological order
            ops.sort_by_key(|(op_num, _)| *op_num);

            let mut prev_timestamp: Option<f64> = None;
            for (op_num, timestamp) in ops {
                if let Some(prev) = prev_timestamp {
                    if timestamp < prev {
                        issues.push(format!(
                            "Client {}: timestamp decreased at op_num {} ({} < {})",
                            client_id, op_num, timestamp, prev
                        ));
                    }
                }
                prev_timestamp = Some(timestamp);
            }
        }

        if issues.is_empty() {
            (true, "All client timestamps are monotonically increasing".to_string())
        } else {
            (false, issues.join("; "))
        }
    }

    /// Invariant 3: Safety - No Overlapping Leadership
    /// - If client A became leader at time T1 with lease L, and client B became leader at T2
    /// - Their lease periods should NOT overlap (unless same client refreshing)
    fn verify_no_overlapping_leadership(
        &self,
        entries: &LogEntries,
        lease_duration: Duration,
    ) -> (bool, String) {
        // Extract leadership events: (timestamp, client_id, lease_end)
        let mut leadership_periods: Vec<(f64, i32, f64)> = Vec::new();
        let lease_secs = lease_duration.as_secs_f64();

        for ((_, client_id, _), (op_type, success, timestamp, became_leader)) in entries {
            if *op_type == OP_TRY_BECOME_LEADER && *success && *became_leader {
                let lease_end = *timestamp + lease_secs;
                leadership_periods.push((*timestamp, *client_id, lease_end));
            }
        }

        // Sort by start timestamp
        leadership_periods.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        let mut issues = Vec::new();

        // Check for overlaps between different clients
        for i in 0..leadership_periods.len() {
            for j in (i + 1)..leadership_periods.len() {
                let (start_i, client_i, end_i) = leadership_periods[i];
                let (start_j, client_j, _end_j) = leadership_periods[j];

                // Different clients with overlapping periods
                if client_i != client_j && start_j < end_i {
                    issues.push(format!(
                        "SAFETY VIOLATION: Client {} (lease {:.3}-{:.3}) overlaps with Client {} (start {:.3})",
                        client_i, start_i, end_i, client_j, start_j
                    ));
                }
            }
        }

        if issues.is_empty() {
            (
                true,
                format!(
                    "No overlapping leadership periods ({} leadership events checked)",
                    leadership_periods.len()
                ),
            )
        } else {
            (false, issues.join("; "))
        }
    }

    /// Invariant 4: Ballot Conservation Law (AtomicOps-style dual accumulation)
    /// - Count successful leadership claims from logs
    /// - Compare to actual ballot in FDB
    fn verify_ballot_conservation(
        &self,
        entries: &LogEntries,
        snapshot: &DatabaseSnapshot,
    ) -> (bool, String) {
        // Count leadership claims from logs (expected ballot increments)
        let expected_ballot: u64 = entries
            .values()
            .filter(|(op_type, success, _, became_leader)| {
                *op_type == OP_TRY_BECOME_LEADER && *success && *became_leader
            })
            .count() as u64;

        // Get actual ballot from FDB
        let actual_ballot = snapshot
            .leader_state
            .as_ref()
            .map(|l| l.ballot)
            .unwrap_or(0);

        if expected_ballot == actual_ballot {
            (
                true,
                format!(
                    "Ballot conservation holds: expected={}, actual={}",
                    expected_ballot, actual_ballot
                ),
            )
        } else {
            (
                false,
                format!(
                    "Ballot conservation VIOLATED: expected={}, actual={}, diff={}",
                    expected_ballot,
                    actual_ballot,
                    (actual_ballot as i64 - expected_ballot as i64).abs()
                ),
            )
        }
    }

    /// Invariant 5: Candidate Registration Consistency
    /// - All clients that sent heartbeats should be registered as candidates
    fn verify_candidate_consistency(
        &self,
        stats: &BTreeMap<i32, ClientStats>,
        snapshot: &DatabaseSnapshot,
    ) -> (bool, String) {
        let mut issues = Vec::new();

        // Build set of registered candidate process_ids
        let registered: std::collections::BTreeSet<String> = snapshot
            .candidates
            .iter()
            .map(|c| c.process_id.clone())
            .collect();

        // Check all clients that heartbeated are registered
        for (client_id, client_stats) in stats {
            if client_stats.heartbeat_count > 0 {
                let process_id = format!("process_{}", client_id);
                if !registered.contains(&process_id) {
                    issues.push(format!(
                        "Client {} sent {} heartbeats but not in candidate list",
                        client_id, client_stats.heartbeat_count
                    ));
                }
            }
        }

        // Check candidates have non-zero versionstamps
        for candidate in &snapshot.candidates {
            if candidate.versionstamp == [0u8; 12] {
                issues.push(format!(
                    "Candidate {} has zero versionstamp",
                    candidate.process_id
                ));
            }
        }

        if issues.is_empty() {
            (
                true,
                format!(
                    "All {} candidates are consistent",
                    snapshot.candidates.len()
                ),
            )
        } else {
            (false, issues.join("; "))
        }
    }

    /// Invariant 6: Leader Is Registered Candidate
    /// - If there's a current leader, they must exist in the candidates list
    fn verify_leader_is_candidate(&self, snapshot: &DatabaseSnapshot) -> (bool, String) {
        match &snapshot.leader_state {
            Some(leader) => {
                let leader_in_candidates = snapshot
                    .candidates
                    .iter()
                    .any(|c| c.process_id == leader.leader_id);

                if leader_in_candidates {
                    // Also verify versionstamp matches
                    let candidate = snapshot
                        .candidates
                        .iter()
                        .find(|c| c.process_id == leader.leader_id);

                    if let Some(c) = candidate {
                        if c.versionstamp == leader.versionstamp {
                            (
                                true,
                                format!(
                                    "Leader {} is registered candidate with matching versionstamp",
                                    leader.leader_id
                                ),
                            )
                        } else {
                            (
                                false,
                                format!(
                                    "Leader {} versionstamp mismatch: leader={:?}, candidate={:?}",
                                    leader.leader_id, leader.versionstamp, c.versionstamp
                                ),
                            )
                        }
                    } else {
                        (false, format!("Leader {} not found in candidates", leader.leader_id))
                    }
                } else {
                    (
                        false,
                        format!("Leader {} is NOT in candidate list", leader.leader_id),
                    )
                }
            }
            None => (true, "No leader to verify".to_string()),
        }
    }

    /// Invariant 7: Operation Sequencing
    /// - Each client should have exactly one registration (first operation)
    /// - Registrations should happen before heartbeats
    fn verify_operation_sequencing(
        &self,
        entries: &LogEntries,
        stats: &BTreeMap<i32, ClientStats>,
    ) -> (bool, String) {
        let mut issues = Vec::new();

        // Check each client has exactly one registration
        for (client_id, client_stats) in stats {
            if client_stats.register_count != 1 {
                issues.push(format!(
                    "Client {} has {} registrations (expected 1)",
                    client_id, client_stats.register_count
                ));
            }
        }

        // Check registration is first op for each client
        let mut client_first_op: BTreeMap<i32, (u64, i64)> = BTreeMap::new();
        for ((_, client_id, op_num), (op_type, _, _, _)) in entries {
            let entry = client_first_op.entry(*client_id).or_insert((*op_num, *op_type));
            if *op_num < entry.0 {
                *entry = (*op_num, *op_type);
            }
        }

        for (client_id, (first_op_num, first_op_type)) in &client_first_op {
            if *first_op_type != OP_REGISTER {
                let op_name = OpType::from_i64(*first_op_type)
                    .map(|o| o.as_str())
                    .unwrap_or("Unknown");
                issues.push(format!(
                    "Client {}: first operation is {} (op_num={}), not Register",
                    client_id, op_name, first_op_num
                ));
            }
        }

        if issues.is_empty() {
            (
                true,
                "All clients have correct operation sequencing".to_string(),
            )
        } else {
            (false, issues.join("; "))
        }
    }

    /// Invariant 8: Error Rate Bounds
    /// - Error rate should be below threshold
    fn verify_error_rate(&self, stats: &BTreeMap<i32, ClientStats>) -> (bool, String) {
        let total_ops: usize = stats
            .values()
            .map(|s| s.register_count + s.heartbeat_count + s.leadership_attempt_count)
            .sum();
        let total_errors: usize = stats.values().map(|s| s.error_count).sum();

        if total_ops == 0 {
            return (true, "No operations to check".to_string());
        }

        let error_rate = total_errors as f64 / total_ops as f64;

        if error_rate <= MAX_ERROR_RATE {
            (
                true,
                format!(
                    "Error rate {:.2}% is within threshold ({:.0}%)",
                    error_rate * 100.0,
                    MAX_ERROR_RATE * 100.0
                ),
            )
        } else {
            (
                false,
                format!(
                    "Error rate {:.2}% EXCEEDS threshold ({:.0}%): {}/{} ops failed",
                    error_rate * 100.0,
                    MAX_ERROR_RATE * 100.0,
                    total_errors,
                    total_ops
                ),
            )
        }
    }

    // ========================================================================
    // DATABASE STATE CAPTURE
    // ========================================================================

    /// Capture database state snapshot for invariant checking
    async fn capture_database_state(&self, db: &SimDatabase) -> DatabaseSnapshot {
        let election = LeaderElection::new(self.election_subspace.clone());
        let current_time = Duration::from_secs_f64(self.context.now());

        // Read leader state
        let leader_state: Option<LeaderState> = db
            .run(|trx, _| {
                let election = election.clone();
                async move {
                    election
                        .get_leader(&trx, current_time)
                        .await
                        .map_err(|e| FdbBindingError::CustomError(e.to_string().into()))
                }
            })
            .await
            .ok()
            .flatten();

        // Read all candidates
        let candidates: Vec<CandidateInfo> = db
            .run(|trx, _| {
                let election = election.clone();
                async move {
                    election
                        .list_candidates(&trx, current_time)
                        .await
                        .map_err(|e| FdbBindingError::CustomError(e.to_string().into()))
                }
            })
            .await
            .unwrap_or_default();

        // Read config
        let config: Option<ElectionConfig> = db
            .run(|trx, _| {
                let election = election.clone();
                async move {
                    election
                        .read_config(&trx)
                        .await
                        .map_err(|e| FdbBindingError::CustomError(e.to_string().into()))
                }
            })
            .await
            .ok();

        DatabaseSnapshot {
            leader_state,
            candidates,
            config,
        }
    }

    // ========================================================================
    // MAIN INVARIANT CHECK RUNNER
    // ========================================================================

    /// Run all invariant checks and return results
    fn run_all_invariant_checks(
        &self,
        entries: &LogEntries,
        stats: &BTreeMap<i32, ClientStats>,
        snapshot: &DatabaseSnapshot,
    ) -> CheckResult {
        let lease_duration = snapshot
            .config
            .as_ref()
            .map(|c| c.lease_duration)
            .unwrap_or(Duration::from_secs(self.heartbeat_timeout_secs));

        let mut results: Vec<(&'static str, bool, String)> = Vec::new();

        // Run each invariant check
        let (pass, detail) = self.verify_log_completeness(entries, stats);
        results.push(("LogCompleteness", pass, detail));

        let (pass, detail) = self.verify_timestamp_monotonicity(entries);
        results.push(("TimestampMonotonicity", pass, detail));

        let (pass, detail) = self.verify_no_overlapping_leadership(entries, lease_duration);
        results.push(("NoOverlappingLeadership", pass, detail));

        let (pass, detail) = self.verify_ballot_conservation(entries, snapshot);
        results.push(("BallotConservation", pass, detail));

        let (pass, detail) = self.verify_candidate_consistency(stats, snapshot);
        results.push(("CandidateConsistency", pass, detail));

        let (pass, detail) = self.verify_leader_is_candidate(snapshot);
        results.push(("LeaderIsCandidate", pass, detail));

        let (pass, detail) = self.verify_operation_sequencing(entries, stats);
        results.push(("OperationSequencing", pass, detail));

        let (pass, detail) = self.verify_error_rate(stats);
        results.push(("ErrorRate", pass, detail));

        // Log each result
        for (name, passed, detail) in &results {
            if *passed {
                self.trace_invariant_pass(name, detail);
            } else {
                self.trace_invariant_fail(name, "invariant holds", detail);
            }
        }

        let passed = results.iter().filter(|(_, p, _)| *p).count();
        let failed = results.len() - passed;

        self.trace_check_summary(passed, failed);

        CheckResult {
            passed,
            failed,
            results,
        }
    }
}

impl RustWorkload for LeaderElectionWorkload {
    async fn setup(&mut self, db: SimDatabase) {
        self.context.trace(
            Severity::Info,
            "LeaderElectionSetup",
            details![
                "Layer" => "Rust",
                "Client" => self.client_id,
                "Phase" => "Setup"
            ],
        );

        // Client 0 initializes the leader election
        if self.client_id == 0 {
            let election = LeaderElection::new(self.election_subspace.clone());
            let config = ElectionConfig::with_lease_duration(Duration::from_secs(
                self.heartbeat_timeout_secs,
            ));

            let result = db
                .run(|mut trx, _maybe_committed| {
                    let election = election.clone();
                    let config = config.clone();
                    async move {
                        trx.set_option(TransactionOption::AutomaticIdempotency)?;
                        election
                            .initialize_with_config(&mut trx, config)
                            .await
                            .map_err(|e| FdbBindingError::CustomError(e.to_string().into()))?;
                        Ok(())
                    }
                })
                .await;

            if let Err(e) = result {
                self.context.trace(
                    Severity::Error,
                    "LeaderElectionInitFailed",
                    details!["Error" => format!("{:?}", e)],
                );
            }
        }

        // All clients register their process
        let election = LeaderElection::new(self.election_subspace.clone());
        let process_id = self.process_id.clone();
        let timestamp = Duration::from_secs_f64(self.context.now());

        let result = db
            .run(|mut trx, _maybe_committed| {
                let election = election.clone();
                let process_id = process_id.clone();
                async move {
                    trx.set_option(TransactionOption::AutomaticIdempotency)?;
                    election
                        .register_candidate(&mut trx, &process_id, 0, timestamp)
                        .await
                        .map_err(|e| FdbBindingError::CustomError(e.to_string().into()))
                }
            })
            .await;

        // Retrieve the versionstamp after successful registration
        if result.is_ok() {
            let election = LeaderElection::new(self.election_subspace.clone());
            let process_id = self.process_id.clone();

            let versionstamp_result: Result<Option<CandidateInfo>, _> = db
                .run(|trx, _maybe_committed| {
                    let election = election.clone();
                    let process_id = process_id.clone();
                    async move {
                        election
                            .get_candidate(&trx, &process_id)
                            .await
                            .map_err(|e| FdbBindingError::CustomError(e.to_string().into()))
                    }
                })
                .await;

            if let Ok(Some(candidate)) = versionstamp_result {
                self.versionstamp = Some(candidate.versionstamp);
            }
        }

        // Log the registration
        self.log_operation(&db, OpType::Register, result.is_ok(), false)
            .await;

        self.context.trace(
            Severity::Info,
            "ProcessRegistered",
            details![
                "Layer" => "Rust",
                "Client" => self.client_id,
                "ProcessId" => self.process_id.clone(),
                "Success" => result.is_ok()
            ],
        );
    }

    async fn start(&mut self, db: SimDatabase) {
        self.context.trace(
            Severity::Info,
            "LeaderElectionStart",
            details![
                "Layer" => "Rust",
                "Client" => self.client_id,
                "Phase" => "Start",
                "OperationCount" => self.operation_count
            ],
        );

        let election = LeaderElection::new(self.election_subspace.clone());

        // Count-based loop (not time-based - simulation time only advances on async ops)
        for _ in 0..self.operation_count {
            let current_time = self.context.now();
            let timestamp = Duration::from_secs_f64(current_time);

            // Send heartbeat
            {
                let process_id = self.process_id.clone();
                let election = election.clone();

                let result = db
                    .run(|mut trx, _maybe_committed| {
                        let election = election.clone();
                        let process_id = process_id.clone();
                        async move {
                            trx.set_option(TransactionOption::AutomaticIdempotency)?;
                            election
                                .heartbeat_candidate(&mut trx, &process_id, 0, timestamp)
                                .await
                                .map_err(|e| FdbBindingError::CustomError(e.to_string().into()))?;
                            Ok(())
                        }
                    })
                    .await;

                if result.is_ok() {
                    self.heartbeat_count += 1;
                } else {
                    self.error_count += 1;
                }

                self.log_operation(&db, OpType::Heartbeat, result.is_ok(), false)
                    .await;
            }

            // Try to become leader
            {
                let process_id = self.process_id.clone();
                let election = election.clone();
                let versionstamp = self.versionstamp.unwrap_or([0u8; 12]);

                let result: Result<Option<LeaderState>, _> = db
                    .run(|mut trx, _maybe_committed| {
                        let election = election.clone();
                        let process_id = process_id.clone();
                        async move {
                            trx.set_option(TransactionOption::AutomaticIdempotency)?;
                            election
                                .try_claim_leadership(
                                    &mut trx,
                                    &process_id,
                                    0,
                                    versionstamp,
                                    timestamp,
                                )
                                .await
                                .map_err(|e| FdbBindingError::CustomError(e.to_string().into()))
                        }
                    })
                    .await;

                self.leadership_attempts += 1;
                let became_leader = matches!(&result, Ok(Some(_)));
                if became_leader {
                    self.times_became_leader += 1;
                    self.context.trace(
                        Severity::Info,
                        "BecameLeader",
                        details![
                            "Layer" => "Rust",
                            "Client" => self.client_id,
                            "ProcessId" => self.process_id.clone(),
                            "Timestamp" => current_time
                        ],
                    );
                }
                if result.is_err() {
                    self.error_count += 1;
                }

                self.log_operation(&db, OpType::TryBecomeLeader, result.is_ok(), became_leader)
                    .await;
            }
        }

        self.context.trace(
            Severity::Info,
            "LeaderElectionStartComplete",
            details![
                "Layer" => "Rust",
                "Client" => self.client_id,
                "HeartbeatCount" => self.heartbeat_count,
                "LeadershipAttempts" => self.leadership_attempts,
                "TimesBecameLeader" => self.times_became_leader
            ],
        );
    }

    async fn check(&mut self, db: SimDatabase) {
        self.trace_check_start();

        // Only client 0 performs verification
        if self.client_id != 0 {
            return;
        }

        // Step 1: Capture database state snapshot (AtomicOps-style)
        let snapshot = self.capture_database_state(&db).await;

        // Step 2: Dump database state for debugging
        self.dump_config(&snapshot);
        self.dump_leader_state(&snapshot);
        self.dump_candidates(&snapshot);

        // Step 3: Read all log entries
        let log_subspace = self.log_subspace.clone();
        let entries_result = db
            .run(|trx, _maybe_committed| {
                let log_subspace = log_subspace.clone();
                async move {
                    let mut all_entries: LogEntries = BTreeMap::new();

                    let range = RangeOption::from(log_subspace.range());
                    let kvs: Vec<_> = trx
                        .get_ranges_keyvalues(range, false)
                        .try_collect()
                        .await
                        .map_err(FdbBindingError::from)?;

                    for kv in kvs.iter() {
                        // Unpack key: (client_id, op_num)
                        let key_tuple: (i32, u64) = log_subspace
                            .unpack(kv.key())
                            .map_err(|e| FdbBindingError::CustomError(e.to_string().into()))?;
                        let (client_id, op_num) = key_tuple;

                        // Unpack value: (op_type, success, timestamp, became_leader)
                        let value_tuple: (i64, bool, f64, bool) = unpack(kv.value())
                            .map_err(|e| FdbBindingError::CustomError(e.to_string().into()))?;

                        // Key for sorting: (timestamp_millis, client_id, op_num)
                        let timestamp_millis = (value_tuple.2 * 1000.0) as i64;
                        all_entries.insert((timestamp_millis, client_id, op_num), value_tuple);
                    }

                    Ok(all_entries)
                }
            })
            .await;

        let entries = match entries_result {
            Ok(e) => e,
            Err(e) => {
                self.context.trace(
                    Severity::Error,
                    "LogEntriesReadFailed",
                    details![
                        "Layer" => "Rust",
                        "Error" => format!("{:?}", e)
                    ],
                );
                return;
            }
        };

        // Step 4: Dump log entries for debugging (AtomicOps dumpLogKV style)
        self.dump_log_entries(&entries);

        // Step 5: Extract statistics and log them
        let stats = self.extract_client_stats(&entries);
        self.log_statistics(&entries, &stats);

        // Step 6: Run all invariant checks
        let result = self.run_all_invariant_checks(&entries, &stats, &snapshot);

        // Step 7: Final summary with pass/fail status
        if result.failed > 0 {
            self.context.trace(
                Severity::Error,
                "LeaderElectionCheckFailed",
                details![
                    "Layer" => "Rust",
                    "InvariantsPassed" => result.passed,
                    "InvariantsFailed" => result.failed,
                    "FailedInvariants" => result.results.iter()
                        .filter(|(_, p, _)| !*p)
                        .map(|(name, _, _)| *name)
                        .collect::<Vec<_>>()
                        .join(", ")
                ],
            );
        } else {
            self.context.trace(
                Severity::Info,
                "LeaderElectionCheckPassed",
                details![
                    "Layer" => "Rust",
                    "InvariantsPassed" => result.passed,
                    "Message" => "All invariants verified successfully"
                ],
            );
        }
    }

    fn get_metrics(&self, mut out: Metrics) {
        out.extend([
            Metric::val("heartbeat_count", self.heartbeat_count as f64),
            Metric::val("leadership_attempts", self.leadership_attempts as f64),
            Metric::val("times_became_leader", self.times_became_leader as f64),
            Metric::val("error_count", self.error_count as f64),
            Metric::val("op_count", self.op_num as f64),
        ]);
    }

    fn get_check_timeout(&self) -> f64 {
        5000.0
    }
}
