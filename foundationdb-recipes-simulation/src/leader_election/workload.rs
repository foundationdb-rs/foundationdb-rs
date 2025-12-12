//! RustWorkload trait implementation for LeaderElectionWorkload

use foundationdb::{
    options::TransactionOption,
    recipes::leader_election::{ElectionConfig, LeaderElection},
    FdbBindingError,
};
use foundationdb_simulation::{details, Metric, Metrics, RustWorkload, Severity, SimDatabase};

use super::types::{op_types, ProcessState};
use super::LeaderElectionWorkload;

impl RustWorkload for LeaderElectionWorkload {
    async fn setup(&mut self, db: SimDatabase) {
        self.context.trace(
            Severity::Info,
            "LeaderElection setup starting",
            details![
                "Layer" => "Rust",
                "Client" => self.client_id,
                "InitialProcesses" => self.initial_processes
            ],
        );

        // Client 0 initializes the election config
        if self.client_id == 0 {
            let election = LeaderElection::new(self.subspace.clone());
            let config = ElectionConfig {
                heartbeat_timeout: self.heartbeat_timeout,
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
                            .map_err(FdbBindingError::from)
                    }
                })
                .await;

            if let Err(e) = result {
                self.context.trace(
                    Severity::Error,
                    "Failed to initialize election",
                    details![
                        "Layer" => "Rust",
                        "Client" => self.client_id,
                        "Error" => format!("{:?}", e)
                    ],
                );
            }
        }

        // All clients register initial processes
        let election = LeaderElection::new(self.subspace.clone());
        for _ in 0..self.initial_processes {
            let uuid = self.generate_process_uuid();
            let timestamp = self.current_time();

            let uuid_clone = uuid.clone();
            let log_key = self.log_key(self.op_num);
            let log_entry = self.pack_log_entry(op_types::REGISTER, &uuid, timestamp, &[]);
            self.op_num += 1;

            let result = db
                .run(|mut trx, _maybe_committed| {
                    let election = election.clone();
                    let uuid = uuid_clone.clone();
                    let log_key = log_key.clone();
                    let log_entry = log_entry.clone();
                    async move {
                        trx.set_option(TransactionOption::AutomaticIdempotency)?;
                        trx.set(&log_key, &log_entry);
                        election
                            .register_process(&mut trx, &uuid, timestamp)
                            .await
                            .map_err(FdbBindingError::from)
                    }
                })
                .await;

            match result {
                Ok(_) => {
                    self.processes.insert(
                        uuid.clone(),
                        ProcessState {
                            uuid,
                            last_heartbeat: timestamp,
                            is_alive: true,
                        },
                    );
                    self.registrations += 1;
                }
                Err(e) => {
                    self.context.trace(
                        Severity::Warn,
                        "Failed to register process",
                        details![
                            "Layer" => "Rust",
                            "Client" => self.client_id,
                            "Error" => format!("{:?}", e)
                        ],
                    );
                }
            }
        }

        self.context.trace(
            Severity::Info,
            "LeaderElection setup complete",
            details![
                "Layer" => "Rust",
                "Client" => self.client_id,
                "RegisteredProcesses" => self.processes.len()
            ],
        );
    }

    async fn start(&mut self, db: SimDatabase) {
        self.context.trace(
            Severity::Info,
            "LeaderElection workload starting",
            details![
                "Layer" => "Rust",
                "Client" => self.client_id,
                "Duration" => self.test_duration
            ],
        );

        let start_time = self.context.now();
        let election = LeaderElection::new(self.subspace.clone());

        while self.context.now() - start_time < self.test_duration {
            self.total_operations += 1;

            let op_roll = self.random_range(100);
            let is_nemesis_client = self.client_id == 0;

            let result: Result<(), String> = if is_nemesis_client && op_roll < 10 {
                self.execute_nemesis_operation(&db, &election).await
            } else if op_roll < 15 {
                self.execute_lifecycle_operation(&db, &election).await
            } else {
                self.execute_normal_operation(&db, &election).await
            };

            match result {
                Ok(_) => self.successful_operations += 1,
                Err(e) => {
                    self.failed_operations += 1;
                    self.context.trace(
                        Severity::Debug,
                        "Operation failed",
                        details![
                            "Layer" => "Rust",
                            "Client" => self.client_id,
                            "Error" => e
                        ],
                    );
                }
            }
        }

        self.context.trace(
            Severity::Info,
            "LeaderElection workload complete",
            details![
                "Layer" => "Rust",
                "Client" => self.client_id,
                "TotalOps" => self.total_operations,
                "SuccessOps" => self.successful_operations,
                "FailedOps" => self.failed_operations
            ],
        );
    }

    async fn check(&mut self, db: SimDatabase) {
        if self.client_id != 0 {
            return;
        }

        self.context.trace(
            Severity::Info,
            "LeaderElection check starting",
            details!["Layer" => "Rust", "Client" => self.client_id],
        );

        let check_result = self.verify_invariants(&db).await;

        match check_result {
            Ok(_) => {
                self.context.trace(
                    Severity::Info,
                    "LeaderElection invariants verified",
                    details![
                        "Layer" => "Rust",
                        "Client" => self.client_id,
                        "LeadershipClaims" => self.leadership_claims
                    ],
                );
            }
            Err(e) => {
                self.context.trace(
                    Severity::Error,
                    "LeaderElection invariant violation",
                    details![
                        "Layer" => "Rust",
                        "Client" => self.client_id,
                        "Error" => e
                    ],
                );
            }
        }
    }

    fn get_metrics(&self, mut out: Metrics) {
        out.extend([
            Metric::val("total_operations", self.total_operations as f64),
            Metric::val("successful_operations", self.successful_operations as f64),
            Metric::val("failed_operations", self.failed_operations as f64),
            Metric::val("leadership_claims", self.leadership_claims as f64),
            Metric::val("registrations", self.registrations as f64),
            Metric::val("heartbeats", self.heartbeats as f64),
            Metric::val("time_anomalies_injected", self.time_anomalies_injected as f64),
        ]);
    }

    fn get_check_timeout(&self) -> f64 {
        30000.0
    }
}
