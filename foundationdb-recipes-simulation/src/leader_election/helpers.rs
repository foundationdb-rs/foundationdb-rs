//! Tracing, statistics, and database capture helpers for leader election simulation.

use std::collections::BTreeMap;
use std::time::Duration;

use foundationdb::{
    recipes::leader_election::{CandidateInfo, ElectionConfig, LeaderElection, LeaderState},
    FdbBindingError,
};
use foundationdb_simulation::{details, Severity, SimDatabase};

use super::types::{ClientStats, DatabaseSnapshot, LogEntries, OpType};
use super::LeaderElectionWorkload;

impl LeaderElectionWorkload {
    // ========================================================================
    // TRACE HELPERS
    // ========================================================================

    pub(crate) fn trace_check_start(&self) {
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

    pub(crate) fn trace_invariant_pass(&self, name: &str, details_str: &str) {
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

    pub(crate) fn trace_invariant_fail(&self, name: &str, expected: &str, actual: &str) {
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

    pub(crate) fn trace_check_summary(&self, passed: usize, failed: usize) {
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
    pub(crate) fn dump_log_entries(&self, entries: &LogEntries) {
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
    pub(crate) fn dump_leader_state(&self, snapshot: &DatabaseSnapshot) {
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
    pub(crate) fn dump_candidates(&self, snapshot: &DatabaseSnapshot) {
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
    pub(crate) fn dump_config(&self, snapshot: &DatabaseSnapshot) {
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
    pub(crate) fn extract_client_stats(&self, entries: &LogEntries) -> BTreeMap<i32, ClientStats> {
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
    pub(crate) fn log_statistics(&self, entries: &LogEntries, stats: &BTreeMap<i32, ClientStats>) {
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
    // DATABASE STATE CAPTURE
    // ========================================================================

    /// Capture database state snapshot for invariant checking
    pub(crate) async fn capture_database_state(&self, db: &SimDatabase) -> DatabaseSnapshot {
        let election = LeaderElection::new(self.election_subspace.clone());
        let current_time = Duration::from_secs_f64(self.context.now());

        // Read leader state WITHOUT lease filtering (for invariant checking)
        // We need the raw state to verify ballot conservation even after lease expires
        let leader_state: Option<LeaderState> = db
            .run(|trx, _| {
                let election = election.clone();
                async move {
                    election
                        .get_leader_raw(&trx)
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
}
