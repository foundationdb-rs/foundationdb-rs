//! RustWorkload trait implementation for leader election simulation.
//!
//! Contains the three main workload phases:
//! - setup: Initialize election and register candidates
//! - start: Execute heartbeats and leadership attempts
//! - check: Verify invariants against logged operations

use std::time::Duration;

use foundationdb::{
    options::{MutationType, TransactionOption},
    recipes::leader_election::{ElectionConfig, LeaderElection},
    tuple::{pack, unpack, Versionstamp},
    FdbBindingError, RangeOption,
};
use foundationdb_simulation::{details, Metric, Metrics, RustWorkload, Severity, SimDatabase};
use futures::TryStreamExt;

use super::types::{
    LogEntries, LogEntry, OP_HEARTBEAT, OP_REGISTER, OP_RESIGN, OP_TRY_BECOME_LEADER,
};
use super::LeaderElectionWorkload;

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

        // Log clock skew configuration for this client
        self.context.trace(
            Severity::Info,
            "ClockSkewConfig",
            details![
                "Layer" => "Rust",
                "ClientId" => self.client_id,
                "ClockSkewLevel" => format!("{:?}", self.clock_skew_level),
                "ClockOffsetSecs" => format!("{:.4}", self.clock_offset_secs),
                "ClockTimerTime" => format!("{:.4}", self.clock_timer_time)
            ],
        );

        // Client 0 initializes the leader election
        if self.client_id == 0 {
            let election = LeaderElection::new(self.election_subspace.clone());
            let config = ElectionConfig::with_lease_duration(Duration::from_secs(
                self.heartbeat_timeout_secs,
            ));

            const MAX_INIT_RETRIES: u32 = 10;
            let mut init_success = false;

            for attempt in 1..=MAX_INIT_RETRIES {
                let result = db
                    .run(|trx, _maybe_committed| {
                        let election = election.clone();
                        let config = config.clone();
                        async move {
                            trx.set_option(TransactionOption::AutomaticIdempotency)?;
                            election
                                .initialize_with_config(&trx, config)
                                .await
                                .map_err(FdbBindingError::from)?;
                            Ok(())
                        }
                    })
                    .await;

                match result {
                    Ok(()) => {
                        init_success = true;
                        break;
                    }
                    Err(e) => {
                        self.context.trace(
                            Severity::Warn,
                            "LeaderElectionInitRetry",
                            details![
                                "Attempt" => attempt,
                                "MaxAttempts" => MAX_INIT_RETRIES,
                                "Error" => format!("{:?}", e)
                            ],
                        );
                    }
                }
            }

            if !init_success {
                self.context.trace(
                    Severity::Error,
                    "LeaderElectionInitFailed",
                    details!["Error" => "Exhausted all retry attempts"],
                );
            }

            // Register ALL candidates (Client 0 does this for everyone to avoid race condition)
            for cid in 0..self.client_count {
                let process_id = format!("process_{cid}");
                let timestamp = self.local_time();
                let log_subspace = self.log_subspace.clone();

                let result = db
                    .run(|trx, _maybe_committed| {
                        let election = election.clone();
                        let process_id = process_id.clone();
                        let log_subspace = log_subspace.clone();
                        async move {
                            trx.set_option(TransactionOption::AutomaticIdempotency)?;
                            let reg_result = election
                                .register_candidate(&trx, &process_id, 0, timestamp)
                                .await;

                            // Log with versionstamp-ordered key (FDB commit order)
                            let success = reg_result.is_ok();
                            let log_key = log_subspace.pack_with_versionstamp(&(
                                Versionstamp::incomplete(0),
                                cid,
                                0_u64, // op_num 0 for registration
                            ));
                            // Format: (op_type, success, became_leader, ballot, previous_ballot, lease_expiry_nanos)
                            let log_value = pack(&(OP_REGISTER, success, false, 0_u64, 0_u64, 0_i64));
                            trx.atomic_op(&log_key, &log_value, MutationType::SetVersionstampedKey);

                            reg_result.map_err(FdbBindingError::from)
                        }
                    })
                    .await;

                self.context.trace(
                    Severity::Info,
                    "ProcessRegistered",
                    details![
                        "Layer" => "Rust",
                        "Client" => cid,
                        "ProcessId" => process_id,
                        "Success" => result.is_ok()
                    ],
                );
            }
        }

        // All clients start with op_num = 1 (registration was op 0, done by Client 0)
        self.op_num = 1;
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
            // Use local_time() for clock skew simulation
            let timestamp = self.local_time();
            let current_time = timestamp.as_secs_f64();

            // Send heartbeat
            {
                let process_id = self.process_id.clone();
                let election = election.clone();
                let log_subspace = self.log_subspace.clone();
                let client_id = self.client_id;
                let op_num = self.op_num;

                let result = db
                    .run(|trx, _maybe_committed| {
                        let election = election.clone();
                        let process_id = process_id.clone();
                        let log_subspace = log_subspace.clone();
                        async move {
                            trx.set_option(TransactionOption::AutomaticIdempotency)?;
                            let hb_result = election
                                .heartbeat_candidate(&trx, &process_id, 0, timestamp)
                                .await;

                            // Log with versionstamp-ordered key (FDB commit order)
                            let success = hb_result.is_ok();
                            let log_key = log_subspace.pack_with_versionstamp(&(
                                Versionstamp::incomplete(0),
                                client_id,
                                op_num,
                            ));
                            // Format: (op_type, success, became_leader, ballot, previous_ballot, lease_expiry_nanos)
                            let log_value = pack(&(OP_HEARTBEAT, success, false, 0_u64, 0_u64, 0_i64));
                            trx.atomic_op(&log_key, &log_value, MutationType::SetVersionstampedKey);

                            hb_result.map_err(FdbBindingError::from)
                        }
                    })
                    .await;

                self.op_num += 1;

                if result.is_ok() {
                    self.heartbeat_count += 1;
                } else {
                    self.error_count += 1;
                }
            }

            // Try to become leader
            let became_leader = {
                let process_id = self.process_id.clone();
                let election = election.clone();
                let log_subspace = self.log_subspace.clone();
                let client_id = self.client_id;
                let op_num = self.op_num;

                let result: Result<Option<_>, _> = db
                    .run(|trx, _maybe_committed| {
                        let election = election.clone();
                        let process_id = process_id.clone();
                        let log_subspace = log_subspace.clone();
                        async move {
                            trx.set_option(TransactionOption::AutomaticIdempotency)?;

                            // Get previous ballot before claim attempt
                            let current = election.get_leader_raw(&trx).await.ok().flatten();
                            let previous_ballot = current.map(|l| l.ballot).unwrap_or(0);

                            let claim_result = election
                                .try_claim_leadership(&trx, &process_id, 0, timestamp)
                                .await
                                .map_err(FdbBindingError::from)?;

                            // Extract ballot and lease info from result
                            let (became_leader, ballot, lease_expiry) = match &claim_result {
                                Some(state) => (true, state.ballot, state.lease_expiry_nanos as i64),
                                None => (false, previous_ballot, 0_i64),
                            };

                            // Log with versionstamp-ordered key (FDB commit order)
                            // Format: (op_type, success, became_leader, ballot, previous_ballot, lease_expiry_nanos)
                            let log_key = log_subspace.pack_with_versionstamp(&(
                                Versionstamp::incomplete(0),
                                client_id,
                                op_num,
                            ));
                            let log_value = pack(&(
                                OP_TRY_BECOME_LEADER,
                                true,
                                became_leader,
                                ballot,
                                previous_ballot,
                                lease_expiry,
                            ));
                            trx.atomic_op(&log_key, &log_value, MutationType::SetVersionstampedKey);

                            Ok(claim_result)
                        }
                    })
                    .await;

                self.op_num += 1;
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
                became_leader
            };

            // Randomly resign if we became leader
            if became_leader {
                let rnd_val = self.context.rnd() as f64 / u32::MAX as f64;
                if rnd_val < self.resign_probability {
                    let process_id = self.process_id.clone();
                    let election = election.clone();
                    let log_subspace = self.log_subspace.clone();
                    let client_id = self.client_id;
                    let op_num = self.op_num;

                    let resign_result: Result<bool, _> = db
                        .run(|trx, _maybe_committed| {
                            let election = election.clone();
                            let process_id = process_id.clone();
                            let log_subspace = log_subspace.clone();
                            async move {
                                trx.set_option(TransactionOption::AutomaticIdempotency)?;

                                // Get previous ballot before resign
                                let current = election.get_leader_raw(&trx).await.ok().flatten();
                                let previous_ballot = current.map(|l| l.ballot).unwrap_or(0);

                                let did_resign = election
                                    .resign_leadership(&trx, &process_id)
                                    .await
                                    .map_err(FdbBindingError::from)?;

                                // Log with versionstamp-ordered key (FDB commit order)
                                // Format: (op_type, success, became_leader, ballot, previous_ballot, lease_expiry_nanos)
                                let log_key = log_subspace.pack_with_versionstamp(&(
                                    Versionstamp::incomplete(0),
                                    client_id,
                                    op_num,
                                ));
                                let log_value =
                                    pack(&(OP_RESIGN, did_resign, false, 0_u64, previous_ballot, 0_i64));
                                trx.atomic_op(
                                    &log_key,
                                    &log_value,
                                    MutationType::SetVersionstampedKey,
                                );

                                Ok(did_resign)
                            }
                        })
                        .await;

                    self.op_num += 1;

                    if matches!(resign_result, Ok(true)) {
                        self.resign_count += 1;
                        self.context.trace(
                            Severity::Info,
                            "LeaderResigned",
                            details![
                                "Layer" => "Rust",
                                "Client" => self.client_id,
                                "ProcessId" => self.process_id.clone(),
                                "Timestamp" => current_time
                            ],
                        );
                    }
                    if resign_result.is_err() {
                        self.error_count += 1;
                    }
                }
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
                "TimesBecameLeader" => self.times_became_leader,
                "ResignCount" => self.resign_count
            ],
        );
    }

    async fn check(&mut self, db: SimDatabase) {
        self.trace_check_start();

        // Only client 0 performs verification
        if self.client_id != 0 {
            return;
        }

        // Step 1: Read all log entries and snapshot in a single transaction
        // Capture read version for debugging (like Cycle.actor.cpp)
        let log_subspace = self.log_subspace.clone();
        let election_subspace = self.election_subspace.clone();

        let check_result = db
            .run(|trx, _maybe_committed| {
                let log_subspace = log_subspace.clone();
                let election_subspace = election_subspace.clone();
                async move {
                    // Get read version for debugging
                    let read_version = trx.get_read_version().await?;

                    // Read log entries - already in versionstamp (commit) order!
                    let range = RangeOption::from(log_subspace.range());
                    let kvs: Vec<_> = trx
                        .get_ranges_keyvalues(range, false)
                        .try_collect()
                        .await
                        .map_err(FdbBindingError::from)?;

                    let mut entries: LogEntries = Vec::with_capacity(kvs.len());
                    for kv in kvs.iter() {
                        // Unpack key: (versionstamp, client_id, op_num)
                        let key_tuple: (Versionstamp, i32, u64) = log_subspace
                            .unpack(kv.key())
                            .map_err(FdbBindingError::PackError)?;
                        let (versionstamp, client_id, op_num) = key_tuple;

                        // Unpack value: (op_type, success, became_leader, ballot, previous_ballot, lease_expiry_nanos)
                        let value_tuple: (i64, bool, bool, u64, u64, i64) =
                            unpack(kv.value()).map_err(FdbBindingError::PackError)?;

                        entries.push(LogEntry {
                            versionstamp,
                            client_id,
                            op_num,
                            op_type: value_tuple.0,
                            success: value_tuple.1,
                            became_leader: value_tuple.2,
                            ballot: value_tuple.3,
                            previous_ballot: value_tuple.4,
                            lease_expiry_nanos: value_tuple.5,
                        });
                    }

                    // Read snapshot data
                    let election = LeaderElection::new(election_subspace);
                    let current_time = Duration::from_secs_f64(0.0); // Will use context.now() later

                    let leader_state = election
                        .get_leader_raw(&trx)
                        .await
                        .map_err(FdbBindingError::from)?;

                    let candidates = election
                        .list_candidates(&trx, current_time)
                        .await
                        .map_err(FdbBindingError::from)?;

                    let config = election.read_config(&trx).await.ok();

                    Ok((read_version, entries, leader_state, candidates, config))
                }
            })
            .await;

        let (read_version, entries, leader_state, candidates, config) = match check_result {
            Ok(r) => r,
            Err(e) => {
                self.context.trace(
                    Severity::Error,
                    "CheckPhaseReadFailed",
                    details![
                        "Layer" => "Rust",
                        "Error" => format!("{:?}", e)
                    ],
                );
                return;
            }
        };

        // Build snapshot struct
        let snapshot = super::types::DatabaseSnapshot {
            leader_state,
            candidates,
            config,
        };

        // Log read version and entry count
        self.context.trace(
            Severity::Info,
            "CheckPhaseRead",
            details![
                "Layer" => "Rust",
                "ReadVersion" => read_version,
                "EntryCount" => entries.len(),
                "HasLeader" => snapshot.leader_state.is_some(),
                "CandidateCount" => snapshot.candidates.len()
            ],
        );

        // Step 2: Run invariant checks FIRST (before dumping)
        let result = self.run_all_invariant_checks(&entries, &snapshot);

        // Step 3: Conditional dumping - only on failure (AtomicOps pattern)
        if result.failed > 0 {
            self.context.trace(
                Severity::Error,
                "InvariantFailed_DumpingState",
                details![
                    "Layer" => "Rust",
                    "ReadVersion" => read_version,
                    "FailedCount" => result.failed
                ],
            );
            // Dump state for debugging
            self.dump_config(&snapshot);
            self.dump_leader_state(&snapshot);
            self.dump_candidates(&snapshot);
            self.dump_log_entries(&entries);
        }

        // Step 4: Extract and log statistics
        let stats = self.extract_client_stats(&entries);
        self.log_statistics(&entries, &stats);

        // Step 5: Final summary with pass/fail status
        if result.failed > 0 {
            self.context.trace(
                Severity::Error,
                "LeaderElectionCheckFailed",
                details![
                    "Layer" => "Rust",
                    "ReadVersion" => read_version,
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
                    "ReadVersion" => read_version,
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
            Metric::val("resign_count", self.resign_count as f64),
            Metric::val("error_count", self.error_count as f64),
            Metric::val("op_count", self.op_num as f64),
        ]);
    }

    fn get_check_timeout(&self) -> f64 {
        5000.0
    }
}
