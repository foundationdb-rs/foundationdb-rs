//! Operation implementations for the leader election simulation workload

use foundationdb::{
    options::TransactionOption,
    recipes::leader_election::{ElectionConfig, LeaderElection},
    FdbBindingError,
};
use foundationdb_simulation::SimDatabase;
use std::time::Duration;

use super::types::{op_types, ProcessState, TimeAnomaly};
use super::LeaderElectionWorkload;

impl LeaderElectionWorkload {
    pub(super) async fn execute_normal_operation(
        &mut self,
        db: &SimDatabase,
        election: &LeaderElection,
    ) -> Result<(), String> {
        let op_roll = self.random_range(70);

        match op_roll {
            0..=29 => self.do_heartbeat(db, election).await,
            30..=54 => self.do_try_become_leader(db, election).await,
            55..=64 => self.do_get_current_leader(db, election).await,
            _ => self.do_is_leader(db, election).await,
        }
    }

    pub(super) async fn execute_lifecycle_operation(
        &mut self,
        db: &SimDatabase,
        election: &LeaderElection,
    ) -> Result<(), String> {
        let op_roll = self.random_range(15);

        if op_roll < 10 {
            self.do_register_new(db, election).await
        } else {
            self.do_let_process_die().await
        }
    }

    pub(super) async fn execute_nemesis_operation(
        &mut self,
        db: &SimDatabase,
        election: &LeaderElection,
    ) -> Result<(), String> {
        let op_roll = self.random_range(10);

        match op_roll {
            0..=2 => self.do_disable_election(db, election).await,
            3..=5 => self.do_enable_election(db, election).await,
            6..=7 => self.do_modify_timeout(db, election).await,
            8 => self.do_clear_leader_state(db).await,
            _ => self.do_kill_process(db).await,
        }
    }

    async fn do_heartbeat(
        &mut self,
        db: &SimDatabase,
        election: &LeaderElection,
    ) -> Result<(), String> {
        let alive_processes: Vec<_> = self
            .processes
            .values()
            .filter(|p| p.is_alive)
            .collect();

        if alive_processes.is_empty() {
            return Ok(());
        }

        let idx = self.random_range(alive_processes.len() as u32) as usize;
        let process = alive_processes[idx].clone();

        let anomaly = self.select_time_anomaly();
        let base_time = self.current_time();
        let timestamp = self.apply_time_anomaly(base_time, anomaly, Some(process.last_heartbeat));

        if !matches!(anomaly, TimeAnomaly::Normal) {
            self.time_anomalies_injected += 1;
        }

        let uuid = process.uuid.clone();
        let log_key = self.log_key(self.op_num);
        let log_entry = self.pack_log_entry(op_types::HEARTBEAT, &uuid, timestamp, &[]);
        self.op_num += 1;

        let election = election.clone();
        let result = db
            .run(|mut trx, _maybe_committed| {
                let election = election.clone();
                let uuid = uuid.clone();
                let log_key = log_key.clone();
                let log_entry = log_entry.clone();
                async move {
                    trx.set_option(TransactionOption::AutomaticIdempotency)?;
                    trx.set(&log_key, &log_entry);
                    election
                        .heartbeat(&mut trx, &uuid, timestamp)
                        .await
                        .map_err(FdbBindingError::from)
                }
            })
            .await;

        match result {
            Ok(_) => {
                if let Some(p) = self.processes.get_mut(&uuid) {
                    p.last_heartbeat = timestamp;
                }
                self.heartbeats += 1;
                Ok(())
            }
            Err(e) => Err(format!("{:?}", e)),
        }
    }

    async fn do_try_become_leader(
        &mut self,
        db: &SimDatabase,
        election: &LeaderElection,
    ) -> Result<(), String> {
        let alive_processes: Vec<_> = self
            .processes
            .values()
            .filter(|p| p.is_alive)
            .collect();

        if alive_processes.is_empty() {
            return Ok(());
        }

        let idx = self.random_range(alive_processes.len() as u32) as usize;
        let process = alive_processes[idx].clone();

        let anomaly = self.select_time_anomaly();
        let base_time = self.current_time();
        let timestamp = self.apply_time_anomaly(base_time, anomaly, Some(process.last_heartbeat));

        if !matches!(anomaly, TimeAnomaly::Normal) {
            self.time_anomalies_injected += 1;
        }

        let uuid = process.uuid.clone();
        let log_key = self.log_key(self.op_num);
        self.op_num += 1;

        let election = election.clone();
        let result = db
            .run(|mut trx, _maybe_committed| {
                let election = election.clone();
                let uuid = uuid.clone();
                let log_key = log_key.clone();
                async move {
                    trx.set_option(TransactionOption::AutomaticIdempotency)?;
                    let became_leader = election
                        .try_become_leader(&mut trx, &uuid, timestamp)
                        .await
                        .map_err(FdbBindingError::from)?;

                    if became_leader {
                        let log_entry = foundationdb::tuple::pack(&(
                            op_types::BECAME_LEADER,
                            uuid.as_str(),
                            timestamp.as_nanos() as u64,
                            Vec::<u8>::new(),
                        ));
                        trx.set(&log_key, &log_entry);
                    }

                    Ok(became_leader)
                }
            })
            .await;

        match result {
            Ok(became_leader) => {
                if became_leader {
                    self.leadership_claims += 1;
                }
                Ok(())
            }
            Err(e) => Err(format!("{:?}", e)),
        }
    }

    async fn do_get_current_leader(
        &mut self,
        db: &SimDatabase,
        election: &LeaderElection,
    ) -> Result<(), String> {
        let timestamp = self.current_time();
        let election = election.clone();

        let result = db
            .run(|mut trx, _maybe_committed| {
                let election = election.clone();
                async move {
                    election
                        .get_current_leader(&mut trx, timestamp)
                        .await
                        .map_err(FdbBindingError::from)
                }
            })
            .await;

        result.map(|_| ()).map_err(|e| format!("{:?}", e))
    }

    async fn do_is_leader(
        &mut self,
        db: &SimDatabase,
        election: &LeaderElection,
    ) -> Result<(), String> {
        let alive_processes: Vec<_> = self
            .processes
            .values()
            .filter(|p| p.is_alive)
            .collect();

        if alive_processes.is_empty() {
            return Ok(());
        }

        let idx = self.random_range(alive_processes.len() as u32) as usize;
        let uuid = alive_processes[idx].uuid.clone();
        let timestamp = self.current_time();
        let election = election.clone();

        let result = db
            .run(|mut trx, _maybe_committed| {
                let election = election.clone();
                let uuid = uuid.clone();
                async move {
                    election
                        .is_leader(&mut trx, &uuid, timestamp)
                        .await
                        .map_err(FdbBindingError::from)
                }
            })
            .await;

        result.map(|_| ()).map_err(|e| format!("{:?}", e))
    }

    pub(super) async fn do_register_new(
        &mut self,
        db: &SimDatabase,
        election: &LeaderElection,
    ) -> Result<(), String> {
        let uuid = self.generate_process_uuid();
        let timestamp = self.current_time();

        let log_key = self.log_key(self.op_num);
        let log_entry = self.pack_log_entry(op_types::REGISTER, &uuid, timestamp, &[]);
        self.op_num += 1;

        let election = election.clone();
        let uuid_clone = uuid.clone();
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
                Ok(())
            }
            Err(e) => Err(format!("{:?}", e)),
        }
    }

    async fn do_let_process_die(&mut self) -> Result<(), String> {
        let alive_processes: Vec<_> = self
            .processes
            .values()
            .filter(|p| p.is_alive)
            .map(|p| p.uuid.clone())
            .collect();

        if alive_processes.is_empty() {
            return Ok(());
        }

        let idx = self.random_range(alive_processes.len() as u32) as usize;
        let uuid = &alive_processes[idx];

        if let Some(p) = self.processes.get_mut(uuid) {
            p.is_alive = false;
        }

        Ok(())
    }

    async fn do_disable_election(
        &mut self,
        db: &SimDatabase,
        election: &LeaderElection,
    ) -> Result<(), String> {
        let timestamp = self.current_time();
        let config = ElectionConfig {
            heartbeat_timeout: self.heartbeat_timeout,
            election_enabled: false,
        };

        let log_key = self.log_key(self.op_num);
        let log_entry = self.pack_log_entry(op_types::CONFIG_CHANGE, "disable", timestamp, &[0]);
        self.op_num += 1;

        let election = election.clone();
        let result = db
            .run(|mut trx, _maybe_committed| {
                let election = election.clone();
                let config = config.clone();
                let log_key = log_key.clone();
                let log_entry = log_entry.clone();
                async move {
                    trx.set_option(TransactionOption::AutomaticIdempotency)?;
                    trx.set(&log_key, &log_entry);
                    election
                        .write_config(&mut trx, &config)
                        .await
                        .map_err(FdbBindingError::from)
                }
            })
            .await;

        match result {
            Ok(_) => {
                self.election_enabled = false;
                Ok(())
            }
            Err(e) => Err(format!("{:?}", e)),
        }
    }

    async fn do_enable_election(
        &mut self,
        db: &SimDatabase,
        election: &LeaderElection,
    ) -> Result<(), String> {
        let timestamp = self.current_time();
        let config = ElectionConfig {
            heartbeat_timeout: self.heartbeat_timeout,
            election_enabled: true,
        };

        let log_key = self.log_key(self.op_num);
        let log_entry = self.pack_log_entry(op_types::CONFIG_CHANGE, "enable", timestamp, &[1]);
        self.op_num += 1;

        let election = election.clone();
        let result = db
            .run(|mut trx, _maybe_committed| {
                let election = election.clone();
                let config = config.clone();
                let log_key = log_key.clone();
                let log_entry = log_entry.clone();
                async move {
                    trx.set_option(TransactionOption::AutomaticIdempotency)?;
                    trx.set(&log_key, &log_entry);
                    election
                        .write_config(&mut trx, &config)
                        .await
                        .map_err(FdbBindingError::from)
                }
            })
            .await;

        match result {
            Ok(_) => {
                self.election_enabled = true;
                Ok(())
            }
            Err(e) => Err(format!("{:?}", e)),
        }
    }

    async fn do_modify_timeout(
        &mut self,
        db: &SimDatabase,
        election: &LeaderElection,
    ) -> Result<(), String> {
        let new_timeout = Duration::from_secs((self.random_range(21) + 5) as u64);
        let timestamp = self.current_time();
        let config = ElectionConfig {
            heartbeat_timeout: new_timeout,
            election_enabled: self.election_enabled,
        };

        let log_key = self.log_key(self.op_num);
        let extra = (new_timeout.as_secs() as u8).to_le_bytes().to_vec();
        let log_entry = self.pack_log_entry(op_types::CONFIG_CHANGE, "timeout", timestamp, &extra);
        self.op_num += 1;

        let election = election.clone();
        let result = db
            .run(|mut trx, _maybe_committed| {
                let election = election.clone();
                let config = config.clone();
                let log_key = log_key.clone();
                let log_entry = log_entry.clone();
                async move {
                    trx.set_option(TransactionOption::AutomaticIdempotency)?;
                    trx.set(&log_key, &log_entry);
                    election
                        .write_config(&mut trx, &config)
                        .await
                        .map_err(FdbBindingError::from)
                }
            })
            .await;

        match result {
            Ok(_) => {
                self.heartbeat_timeout = new_timeout;
                Ok(())
            }
            Err(e) => Err(format!("{:?}", e)),
        }
    }

    pub(super) async fn do_clear_leader_state(&mut self, db: &SimDatabase) -> Result<(), String> {
        let timestamp = self.current_time();
        let leader_key = self.subspace.pack(&("leader_state",));

        let log_key = self.log_key(self.op_num);
        let log_entry = self.pack_log_entry(op_types::LEADER_CLEARED, "", timestamp, &[]);
        self.op_num += 1;

        let result = db
            .run(|trx, _maybe_committed| {
                let leader_key = leader_key.clone();
                let log_key = log_key.clone();
                let log_entry = log_entry.clone();
                async move {
                    trx.set_option(TransactionOption::AutomaticIdempotency)?;
                    trx.set(&log_key, &log_entry);
                    trx.clear(&leader_key);
                    Ok::<_, FdbBindingError>(())
                }
            })
            .await;

        result.map_err(|e| format!("{:?}", e))
    }

    pub(super) async fn do_kill_process(&mut self, db: &SimDatabase) -> Result<(), String> {
        let alive_processes: Vec<_> = self
            .processes
            .values()
            .filter(|p| p.is_alive)
            .map(|p| p.uuid.clone())
            .collect();

        if alive_processes.is_empty() {
            return Ok(());
        }

        let idx = self.random_range(alive_processes.len() as u32) as usize;
        let uuid = alive_processes[idx].clone();
        let timestamp = self.current_time();

        let process_key = self.subspace.pack(&("processes", uuid.as_str()));
        let log_key = self.log_key(self.op_num);
        let log_entry = self.pack_log_entry(op_types::PROCESS_KILLED, &uuid, timestamp, &[]);
        self.op_num += 1;

        let result = db
            .run(|trx, _maybe_committed| {
                let process_key = process_key.clone();
                let log_key = log_key.clone();
                let log_entry = log_entry.clone();
                async move {
                    trx.set_option(TransactionOption::AutomaticIdempotency)?;
                    trx.set(&log_key, &log_entry);
                    trx.clear(&process_key);
                    Ok::<_, FdbBindingError>(())
                }
            })
            .await;

        match result {
            Ok(_) => {
                if let Some(p) = self.processes.get_mut(&uuid) {
                    p.is_alive = false;
                }
                Ok(())
            }
            Err(e) => Err(format!("{:?}", e)),
        }
    }
}
