//! Tracing, statistics, and database capture helpers for leader election simulation.

use std::collections::BTreeMap;
use std::time::Duration;

use foundationdb::{
    recipes::leader_election::{CandidateInfo, ElectionConfig, LeaderElection, LeaderState},
    FdbBindingError,
};
use foundationdb_simulation::{details, Severity, SimDatabase};

use super::types::{
    ClientStats, DatabaseSnapshot, LogEntries, OpType, DEFAULT_CLOCK_JITTER_OFFSET,
    DEFAULT_CLOCK_JITTER_RANGE,
};
use super::LeaderElectionWorkload;

impl LeaderElectionWorkload {
    // ========================================================================
    // CLOCK SKEW SIMULATION (mimics FDB's sim2.actor.cpp timer())
    // ========================================================================

    /// Helper to get random f64 in [0, 1) using context.rnd()
    fn rnd_f64(&self) -> f64 {
        self.context.rnd() as f64 / u32::MAX as f64
    }

    /// Get local time with simulated clock skew (mimics FDB's timer())
    ///
    /// FDB strategy from sim2.actor.cpp:
    /// timerTime += random01() * (time + 0.1 - timerTime) / 2.0
    pub(crate) fn local_time(&mut self) -> Duration {
        let real_time = self.context.now();

        // 1. Apply FDB-style timer drift
        // Timer moves partway toward (real_time + base_offset + max_drift)
        let max_ahead = self.clock_skew_level.max_offset_secs();
        let target = real_time + self.clock_offset_secs + max_ahead;
        self.clock_timer_time += self.rnd_f64() * (target - self.clock_timer_time) / 2.0;

        // Ensure timer doesn't go backwards (monotonic-ish)
        self.clock_timer_time = self
            .clock_timer_time
            .max(real_time + self.clock_offset_secs);

        // 2. Calculate offset from real time
        let offset = self.clock_timer_time - real_time;

        // 3. Apply jitter to the OFFSET only (not the entire time)
        // FDB's DELAY_JITTER applies to delay amounts, not absolute times
        let jitter_multiplier =
            DEFAULT_CLOCK_JITTER_OFFSET + DEFAULT_CLOCK_JITTER_RANGE * self.rnd_f64();
        let jittered_offset = offset * jitter_multiplier;

        let local = real_time + jittered_offset;
        Duration::from_secs_f64(local.max(0.0))
    }

    /// Get real simulated time (global truth, for invariant checking)
    pub(crate) fn real_time(&self) -> Duration {
        Duration::from_secs_f64(self.context.now())
    }

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

        // Trace the subspace we're using
        self.context.trace(
            Severity::Info,
            "CaptureStateStart",
            details![
                "Layer" => "Rust",
                "ElectionSubspace" => format!("{:?}", self.election_subspace.bytes()),
                "CurrentTime" => current_time.as_secs_f64()
            ],
        );

        // Read leader state WITHOUT lease filtering (for invariant checking)
        // We need the raw state to verify ballot conservation even after lease expires
        let leader_result = db
            .run(|trx, _| {
                let election = election.clone();
                async move {
                    election
                        .get_leader_raw(&trx)
                        .await
                        .map_err(FdbBindingError::from)
                }
            })
            .await;

        // Trace the leader read result
        self.context.trace(
            Severity::Info,
            "LeaderReadResult",
            details![
                "Layer" => "Rust",
                "Success" => leader_result.is_ok(),
                "HasLeader" => leader_result.as_ref().map(|l| l.is_some()).unwrap_or(false),
                "Error" => leader_result.as_ref().err().map(|e| format!("{:?}", e)).unwrap_or_default()
            ],
        );

        let leader_state: Option<LeaderState> = leader_result.ok().flatten();

        // Read all candidates
        let candidates_result = db
            .run(|trx, _| {
                let election = election.clone();
                async move {
                    election
                        .list_candidates(&trx, current_time)
                        .await
                        .map_err(FdbBindingError::from)
                }
            })
            .await;

        // Trace candidates read result
        self.context.trace(
            Severity::Info,
            "CandidatesReadResult",
            details![
                "Layer" => "Rust",
                "Success" => candidates_result.is_ok(),
                "Count" => candidates_result.as_ref().map(|c| c.len()).unwrap_or(0),
                "Error" => candidates_result.as_ref().err().map(|e| format!("{:?}", e)).unwrap_or_default()
            ],
        );

        let candidates: Vec<CandidateInfo> = candidates_result.unwrap_or_default();

        // Read config
        let config_result = db
            .run(|trx, _| {
                let election = election.clone();
                async move {
                    election
                        .read_config(&trx)
                        .await
                        .map_err(FdbBindingError::from)
                }
            })
            .await;

        // Trace config read result
        self.context.trace(
            Severity::Info,
            "ConfigReadResult",
            details![
                "Layer" => "Rust",
                "Success" => config_result.is_ok(),
                "HasConfig" => config_result.is_ok(),
                "Error" => config_result.as_ref().err().map(|e| format!("{:?}", e)).unwrap_or_default()
            ],
        );

        let config: Option<ElectionConfig> = config_result.ok();

        DatabaseSnapshot {
            leader_state,
            candidates,
            config,
        }
    }
}
