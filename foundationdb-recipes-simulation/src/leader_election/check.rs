//! Tracing, statistics, database capture, and RustWorkload implementation.

use std::collections::BTreeMap;
use std::time::Duration;

use foundationdb::{
    options::TransactionOption,
    recipes::leader_election::{CandidateInfo, ElectionConfig, LeaderElection, LeaderState},
    tuple::{pack, unpack},
    FdbBindingError, RangeOption,
};
use foundationdb_simulation::{details, Metric, Metrics, RustWorkload, Severity, SimDatabase};
use futures::TryStreamExt;

use super::types::{ClientStats, DatabaseSnapshot, LogEntries, OpType};
use super::LeaderElectionWorkload;

impl LeaderElectionWorkload {
    /// Log an operation to the log subspace for verification
    pub(crate) async fn log_operation(
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
                    // Enhanced logging with ballot and lease info for debugging
                    if let Ok(Some(ref state)) = result {
                        self.context.trace(
                            Severity::Info,
                            "BecameLeader",
                            details![
                                "Layer" => "Rust",
                                "Client" => self.client_id,
                                "ProcessId" => self.process_id.clone(),
                                "Timestamp" => current_time,
                                "Ballot" => state.ballot,
                                "LeaseExpirySecs" => state.lease_expiry_nanos as f64 / 1_000_000_000.0
                            ],
                        );
                    }
                } else if result.is_ok() {
                    // Log when leadership claim is rejected (for debugging overlaps)
                    self.context.trace(
                        Severity::Debug,
                        "LeadershipClaimRejected",
                        details![
                            "Layer" => "Rust",
                            "Client" => self.client_id,
                            "ProcessId" => self.process_id.clone(),
                            "Timestamp" => current_time,
                            "Reason" => "Another leader has valid lease"
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

        // Step 5: Extract statistics and log them (for debugging)
        let stats = self.extract_client_stats(&entries);
        self.log_statistics(&entries, &stats);

        // Step 6: Run core invariant checks (Safety + Ballot Conservation)
        let result = self.run_all_invariant_checks(&entries, &snapshot);

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
