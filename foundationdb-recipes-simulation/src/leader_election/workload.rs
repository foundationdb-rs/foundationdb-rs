//! RustWorkload trait implementation for leader election simulation.
//!
//! Contains the three main workload phases:
//! - setup: Initialize election and register candidates
//! - start: Execute heartbeats and leadership attempts
//! - check: Verify invariants against logged operations

use std::collections::BTreeMap;
use std::time::Duration;

use foundationdb::{
    options::TransactionOption,
    recipes::leader_election::{ElectionConfig, LeaderElection},
    tuple::{pack, unpack},
    FdbBindingError, RangeOption,
};
use foundationdb_simulation::{details, Metric, Metrics, RustWorkload, Severity, SimDatabase};
use futures::TryStreamExt;

use super::types::{LogEntries, OP_HEARTBEAT, OP_REGISTER, OP_TRY_BECOME_LEADER};
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
                let process_id = format!("process_{}", cid);
                let timestamp = self.local_time();
                let timestamp_secs = timestamp.as_secs_f64();
                let log_key = self.log_subspace.pack(&(cid, 0_u64)); // op_num 0 for each client

                let result = db
                    .run(|trx, _maybe_committed| {
                        let election = election.clone();
                        let process_id = process_id.clone();
                        let log_key = log_key.clone();
                        async move {
                            trx.set_option(TransactionOption::AutomaticIdempotency)?;
                            let reg_result = election
                                .register_candidate(&trx, &process_id, 0, timestamp)
                                .await;

                            // Log in SAME transaction - atomic with the operation
                            let success = reg_result.is_ok();
                            let log_value = pack(&(OP_REGISTER, success, timestamp_secs, false));
                            trx.set(&log_key, &log_value);

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
                let log_key = self.log_subspace.pack(&(self.client_id, self.op_num));

                let result = db
                    .run(|trx, _maybe_committed| {
                        let election = election.clone();
                        let process_id = process_id.clone();
                        let log_key = log_key.clone();
                        async move {
                            trx.set_option(TransactionOption::AutomaticIdempotency)?;
                            let hb_result = election
                                .heartbeat_candidate(&trx, &process_id, 0, timestamp)
                                .await;

                            // Log in SAME transaction - atomic with the operation
                            let success = hb_result.is_ok();
                            let log_value = pack(&(OP_HEARTBEAT, success, current_time, false));
                            trx.set(&log_key, &log_value);

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
            {
                let process_id = self.process_id.clone();
                let election = election.clone();
                let log_key = self.log_subspace.pack(&(self.client_id, self.op_num));

                let result: Result<Option<_>, _> = db
                    .run(|trx, _maybe_committed| {
                        let election = election.clone();
                        let process_id = process_id.clone();
                        let log_key = log_key.clone();
                        async move {
                            trx.set_option(TransactionOption::AutomaticIdempotency)?;
                            let claim_result = election
                                .try_claim_leadership(&trx, &process_id, 0, timestamp)
                                .await
                                .map_err(FdbBindingError::from)?;

                            // Log in SAME transaction - atomic with the operation
                            // became_leader is true only if we got Some(state)
                            let became_leader = claim_result.is_some();
                            let log_value =
                                pack(&(OP_TRY_BECOME_LEADER, true, current_time, became_leader));
                            trx.set(&log_key, &log_value);

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
                            .map_err(FdbBindingError::PackError)?;
                        let (client_id, op_num) = key_tuple;

                        // Unpack value: (op_type, success, timestamp, became_leader)
                        let value_tuple: (i64, bool, f64, bool) =
                            unpack(kv.value()).map_err(FdbBindingError::PackError)?;

                        // Key for sorting: (timestamp_micros, client_id, op_num)
                        // Use microseconds to avoid sub-millisecond ordering issues
                        let timestamp_micros = (value_tuple.2 * 1_000_000.0) as i64;
                        all_entries.insert((timestamp_micros, client_id, op_num), value_tuple);
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
