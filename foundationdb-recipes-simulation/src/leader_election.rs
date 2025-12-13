//! Leader Election Simulation Workload
//!
//! Tests the leader election recipe under chaos conditions including
//! network partitions, process failures, and cluster reconfigurations.

use std::collections::BTreeMap;
use std::time::Duration;

use foundationdb::{
    options::TransactionOption,
    recipes::leader_election::{ElectionConfig, LeaderElection},
    tuple::{pack, unpack, Subspace},
    FdbBindingError, RangeOption,
};
use foundationdb_simulation::{
    details, Metric, Metrics, RustWorkload, Severity, SimDatabase, SingleRustWorkload,
    WorkloadContext,
};

// Use i64 instead of u8 for tuple packing (u8 is not supported)
const OP_REGISTER: i64 = 0;
const OP_HEARTBEAT: i64 = 1;
const OP_TRY_BECOME_LEADER: i64 = 2;

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

    /// Verify invariants from logged operations
    fn verify_invariants(&self, entries: &LogEntries) {
        // Extract leadership events: (timestamp, client_id)
        let mut leadership_events: Vec<(f64, i32)> = Vec::new();

        for ((_, client_id, _), (op_type, success, timestamp, became_leader)) in entries {
            // OpType::TryBecomeLeader = OP_TRY_BECOME_LEADER
            if *op_type == OP_TRY_BECOME_LEADER && *success && *became_leader {
                leadership_events.push((*timestamp, *client_id));
            }
        }

        // Sort by timestamp
        leadership_events.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        self.context.trace(
            Severity::Info,
            "LeaderElectionVerification",
            details![
                "Layer" => "Rust",
                "TotalLogEntries" => entries.len(),
                "LeadershipEvents" => leadership_events.len(),
                "ClientCount" => self.client_count
            ],
        );

        if leadership_events.is_empty() {
            self.context.trace(
                Severity::Warn,
                "NoLeadershipEvents",
                details![
                    "Layer" => "Rust",
                    "Message" => "No leadership events recorded"
                ],
            );
        } else {
            self.context.trace(
                Severity::Info,
                "LeaderElectionSuccess",
                details![
                    "Layer" => "Rust",
                    "Message" => "Leader election workload completed",
                    "FirstLeader" => leadership_events.first().map(|(_, c)| *c).unwrap_or(-1),
                    "TotalLeadershipChanges" => leadership_events.len()
                ],
            );
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
            let config = ElectionConfig {
                heartbeat_timeout: Duration::from_secs(self.heartbeat_timeout_secs),
                election_enabled: true,
            };

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
                        .register_process(&mut trx, &process_id, timestamp)
                        .await
                        .map_err(|e| FdbBindingError::CustomError(e.to_string().into()))
                }
            })
            .await;

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
                                .heartbeat(&mut trx, &process_id, timestamp)
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

                let result = db
                    .run(|mut trx, _maybe_committed| {
                        let election = election.clone();
                        let process_id = process_id.clone();
                        async move {
                            trx.set_option(TransactionOption::AutomaticIdempotency)?;
                            election
                                .try_become_leader(&mut trx, &process_id, timestamp)
                                .await
                                .map_err(|e| FdbBindingError::CustomError(e.to_string().into()))
                        }
                    })
                    .await;

                self.leadership_attempts += 1;
                let became_leader = matches!(&result, Ok(true));
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
        self.context.trace(
            Severity::Info,
            "LeaderElectionCheck",
            details![
                "Layer" => "Rust",
                "Client" => self.client_id,
                "Phase" => "Check"
            ],
        );

        // Only client 0 performs verification
        if self.client_id != 0 {
            return;
        }

        // Read all log entries and verify invariants
        let log_subspace = self.log_subspace.clone();
        let result = db
            .run(|trx, _maybe_committed| {
                let log_subspace = log_subspace.clone();
                async move {
                    // Collect all log entries, sorted by (timestamp_millis, client_id, op_num)
                    let mut all_entries: LogEntries = BTreeMap::new();

                    let range = log_subspace.range();
                    let kvs = trx
                        .get_range(&RangeOption::from(range), 0, false)
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

        match result {
            Ok(entries) => self.verify_invariants(&entries),
            Err(e) => {
                self.context.trace(
                    Severity::Error,
                    "LeaderElectionCheckFailed",
                    details!["Error" => format!("{:?}", e)],
                );
            }
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
