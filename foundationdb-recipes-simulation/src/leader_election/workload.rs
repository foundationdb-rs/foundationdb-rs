// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Workload and commit-order oracle for poll-based leader election.

use std::{collections::BTreeSet, time::Duration};

use foundationdb::{
    FdbBindingError, RangeOption,
    options::{MutationType, TransactionOption},
    recipes::{
        leader_election::{LeaderElection, Observation, ParticipantId, PollOutcome},
        ranked_register::{Rank, RankedRegister, RankedRegisterError},
    },
    tuple::{Subspace, Versionstamp, pack, unpack},
};
use foundationdb_simulation::{
    Metric, Metrics, RustWorkload, Severity, SimDatabase, SingleRustWorkload, WorkloadContext,
    details,
};
use futures::TryStreamExt;

use super::types::{LogEntry, OP_OBSERVE, OP_POLL, OP_RESIGN, OP_STALE_WRITE, Snapshot};

const OPTIONAL_RESIGN: u32 = 1;
const OPTIONAL_STALE: u32 = 2;
const OPTIONAL_OBSERVER: u32 = 4;
const OPTIONAL_PAUSE: u32 = 8;

pub struct LeaderElectionWorkload {
    context: WorkloadContext,
    client_id: i32,
    operation_count: usize,
    suspicion_duration: Duration,
    election_subspace: Subspace,
    register_subspace: Subspace,
    log_subspace: Subspace,
    participant: ParticipantId,
    observation: Observation,
    last_rank: Option<Rank>,
    stale_rank: Option<Rank>,
    core_stale_write_completed: bool,
    op_num: u64,
    swarm: u32,
    poll_count: u64,
    leader_count: u64,
    committed_resigns: u64,
    run_errors: u64,
}

impl SingleRustWorkload for LeaderElectionWorkload {
    fn new(_name: String, context: WorkloadContext) -> Self {
        let client_id = context.client_id();
        let suspicion_secs = context.get_option("suspicionSecs").unwrap_or(2_u64);
        let now = simulated_now(&context);
        let participant = ParticipantId::new(format!(
            "participant-{client_id}-{}",
            context.get_process_id()
        ))
        .expect("fixed participant IDs are non-empty");

        Self {
            client_id,
            operation_count: context.get_option("operationCount").unwrap_or(40),
            suspicion_duration: Duration::from_secs(suspicion_secs),
            election_subspace: Subspace::all().subspace(&("poll-leader-election",)),
            register_subspace: Subspace::all().subspace(&("poll-leader-register",)),
            log_subspace: Subspace::all().subspace(&("poll-leader-log",)),
            observation: Observation::initial(now),
            last_rank: None,
            stale_rank: None,
            core_stale_write_completed: false,
            op_num: 0,
            swarm: context.rnd(),
            poll_count: 0,
            leader_count: 0,
            committed_resigns: 0,
            run_errors: 0,
            participant,
            context,
        }
    }
}

impl RustWorkload for LeaderElectionWorkload {
    async fn setup(&mut self, _db: SimDatabase) {
        if self.client_id == 0 {
            self.context.trace(
                Severity::Info,
                "PollLeaderElectionSetup",
                details![
                    "Layer" => "Rust",
                    "SuspicionSecs" => self.suspicion_duration.as_secs(),
                    "Protocol" => "poll-fenced-ranked-register"
                ],
            );
        }
    }

    async fn start(&mut self, db: SimDatabase) {
        let election = LeaderElection::new(self.election_subspace.clone(), self.suspicion_duration);
        let register = RankedRegister::new(self.register_subspace.clone());

        for round in 0..self.operation_count {
            self.poll_count += 1;
            let op_num = self.next_op_num();
            let now = simulated_now(&self.context);
            let payload = protected_payload(self.participant.as_str(), op_num);
            let result = run_poll(
                &db,
                election.clone(),
                register.clone(),
                self.log_subspace.clone(),
                self.participant.clone(),
                self.observation.clone(),
                now,
                self.client_id,
                op_num,
                payload,
            )
            .await;

            match result {
                Ok(poll) => {
                    self.observation = poll.observation;
                    if let Some(rank) = poll.leader_rank {
                        if let Some(previous_rank) =
                            self.last_rank.filter(|previous| *previous < rank)
                        {
                            self.stale_rank = Some(previous_rank);
                        }
                        self.last_rank = Some(rank);
                        self.leader_count += 1;
                        if !poll.protected_write_committed {
                            self.protocol_error(
                                "LeaderWriteRejected",
                                op_num,
                                "leader poll committed without its fenced write",
                            );
                        }
                    }
                }
                Err(error) => self.run_diagnostic("PollRunFailed", op_num, error),
            }

            if !self.core_stale_write_completed {
                if let Some(stale_rank) = self.stale_rank {
                    let op_num = self.next_op_num();
                    match run_stale_write(
                        &db,
                        election.clone(),
                        register.clone(),
                        self.log_subspace.clone(),
                        self.participant.as_str().to_owned(),
                        stale_rank,
                        self.client_id,
                        op_num,
                    )
                    .await
                    {
                        Ok(true) => self.protocol_error(
                            "StaleWriteCommitted",
                            op_num,
                            "a stale ranked-register write committed",
                        ),
                        Ok(false) => self.core_stale_write_completed = true,
                        Err(error) => self.run_diagnostic("StaleWriteRunFailed", op_num, error),
                    }
                }
            }

            if self.swarm & OPTIONAL_OBSERVER != 0 && round % 3 == 0 {
                let op_num = self.next_op_num();
                if let Err(error) = run_observer(
                    &db,
                    election.clone(),
                    self.log_subspace.clone(),
                    self.participant.as_str().to_owned(),
                    self.client_id,
                    op_num,
                )
                .await
                {
                    self.run_diagnostic("ObserverRunFailed", op_num, error);
                }
            }

            if self.swarm & OPTIONAL_RESIGN != 0 && round % 5 == 1 {
                if let Some(rank) = self.last_rank {
                    let op_num = self.next_op_num();
                    match run_resign(
                        &db,
                        election.clone(),
                        self.log_subspace.clone(),
                        self.participant.clone(),
                        rank,
                        self.client_id,
                        op_num,
                    )
                    .await
                    {
                        Ok(resigned) => {
                            if resigned {
                                self.committed_resigns += 1;
                            }
                        }
                        Err(error) => self.run_diagnostic("ResignRunFailed", op_num, error),
                    }
                }
            }

            if self.swarm & OPTIONAL_STALE != 0 && round % 5 == 3 {
                let stale_resign_op = self.next_op_num();
                match run_resign(
                    &db,
                    election.clone(),
                    self.log_subspace.clone(),
                    self.participant.clone(),
                    Rank::ZERO,
                    self.client_id,
                    stale_resign_op,
                )
                .await
                {
                    Ok(true) => self.protocol_error(
                        "StaleResignCommitted",
                        stale_resign_op,
                        "a zero-rank stale resignation committed",
                    ),
                    Ok(false) => {}
                    Err(error) => {
                        self.run_diagnostic("StaleResignRunFailed", stale_resign_op, error)
                    }
                }

                if let Some(stale_rank) = self.stale_rank {
                    let op_num = self.next_op_num();
                    match run_stale_write(
                        &db,
                        election.clone(),
                        register.clone(),
                        self.log_subspace.clone(),
                        self.participant.as_str().to_owned(),
                        stale_rank,
                        self.client_id,
                        op_num,
                    )
                    .await
                    {
                        Ok(true) => self.protocol_error(
                            "StaleWriteCommitted",
                            op_num,
                            "a stale ranked-register write committed",
                        ),
                        Ok(false) => {}
                        Err(error) => self.run_diagnostic("StaleWriteRunFailed", op_num, error),
                    }
                }
            }

            let pause = if self.swarm & OPTIONAL_PAUSE != 0 && round % 4 == 0 {
                self.suspicion_duration + Duration::from_millis(1)
            } else {
                Duration::from_millis(1)
            };
            if let Err(error) = self.context.delay(pause).await {
                self.context.trace(
                    Severity::Warn,
                    "SimulationDelayFailed",
                    details!["Client" => self.client_id, "Error" => format!("{error:?}")],
                );
            }
        }
    }

    async fn check(&mut self, db: SimDatabase) {
        if self.client_id != 0 {
            return;
        }

        let log_subspace = self.log_subspace.clone();
        let election = LeaderElection::new(self.election_subspace.clone(), self.suspicion_duration);
        let register = RankedRegister::new(self.register_subspace.clone());
        let check = db
            .run(|txn, _maybe_committed| {
                let log_subspace = log_subspace.clone();
                let election = election.clone();
                let register = register.clone();
                async move {
                    txn.set_option(TransactionOption::AutomaticIdempotency)?;
                    let read_version = txn.get_read_version().await?;
                    let values: Vec<_> = txn
                        .get_ranges_keyvalues(RangeOption::from(log_subspace.range()), false)
                        .try_collect()
                        .await
                        .map_err(FdbBindingError::from)?;
                    let mut entries = Vec::with_capacity(values.len());
                    for value in values {
                        entries.push(decode_log_entry(&log_subspace, value.key(), value.value())?);
                    }
                    let state = election.state(&txn).await.map_err(FdbBindingError::from)?;
                    let protected = register
                        .read(&txn, Rank::ZERO)
                        .await
                        .map_err(ranked_register_error)?;
                    Ok::<_, FdbBindingError>((
                        read_version,
                        entries,
                        Snapshot {
                            generation: state.rank().as_u64(),
                            owner: state.owner().map(|owner| owner.as_str().to_owned()),
                            protected_rank: protected.write_rank().as_u64(),
                            protected_value: protected.into_value(),
                        },
                    ))
                }
            })
            .await;

        match check {
            Ok((read_version, entries, snapshot)) => match replay(&entries, &snapshot) {
                Ok(()) => self.context.trace(
                    Severity::Info,
                    "PollLeaderElectionCheckPassed",
                    details![
                        "ReadVersion" => read_version,
                        "Entries" => entries.len(),
                        "Generation" => snapshot.generation
                    ],
                ),
                Err(error) => self.context.trace(
                    Severity::Error,
                    "PollLeaderElectionInvariantFailed",
                    details![
                        "ReadVersion" => read_version,
                        "Entries" => entries.len(),
                        "Error" => error
                    ],
                ),
            },
            Err(error) => self.context.trace(
                Severity::Error,
                "PollLeaderElectionCheckReadFailed",
                details!["Error" => format!("{error:?}")],
            ),
        }
    }

    fn get_metrics(&self, mut out: Metrics) {
        out.extend([
            Metric::val("poll_count", self.poll_count as f64),
            Metric::val("leader_count", self.leader_count as f64),
            Metric::val("committed_resigns", self.committed_resigns as f64),
            Metric::val("run_errors", self.run_errors as f64),
        ]);
    }

    fn get_check_timeout(&self) -> f64 {
        5000.0
    }
}

impl LeaderElectionWorkload {
    fn next_op_num(&mut self) -> u64 {
        let op_num = self.op_num;
        self.op_num += 1;
        op_num
    }

    fn run_diagnostic(&mut self, event: &str, op_num: u64, error: FdbBindingError) {
        self.run_errors += 1;
        self.context.trace(
            Severity::Warn,
            event,
            details![
                "Client" => self.client_id,
                "OpNum" => op_num,
                "Error" => format!("{error:?}"),
                "MaybeCommitted" => error.get_fdb_error().is_some_and(|fdb| fdb.is_maybe_committed())
            ],
        );
    }

    fn protocol_error(&self, event: &str, op_num: u64, error: &str) {
        self.context.trace(
            Severity::Error,
            event,
            details!["Client" => self.client_id, "OpNum" => op_num, "Error" => error],
        );
    }
}

struct PollRun {
    observation: Observation,
    leader_rank: Option<Rank>,
    protected_write_committed: bool,
}

#[allow(clippy::too_many_arguments)]
async fn run_poll(
    db: &SimDatabase,
    election: LeaderElection,
    register: RankedRegister,
    log_subspace: Subspace,
    participant: ParticipantId,
    previous: Observation,
    now: Duration,
    client_id: i32,
    op_num: u64,
    payload: Vec<u8>,
) -> Result<PollRun, FdbBindingError> {
    db.run(|txn, _maybe_committed| {
        let election = election.clone();
        let register = register.clone();
        let log_subspace = log_subspace.clone();
        let participant = participant.clone();
        let previous = previous.clone();
        let payload = payload.clone();
        async move {
            txn.set_option(TransactionOption::AutomaticIdempotency)?;
            let prior = election.state(&txn).await.map_err(FdbBindingError::from)?;
            let poll = election
                .poll(&txn, &participant, &previous, now)
                .await
                .map_err(FdbBindingError::from)?;
            let (
                generation,
                owner,
                requested_rank,
                result,
                takeover,
                protected_write_committed,
                observed_write_rank,
                observed_value,
            ) = match poll.outcome() {
                PollOutcome::Leader { rank, takeover } => {
                    let read = register
                        .read(&txn, *rank)
                        .await
                        .map_err(ranked_register_error)?;
                    let write = register
                        .write(&txn, *rank, &payload)
                        .await
                        .map_err(ranked_register_error)?;
                    (
                        rank.as_u64(),
                        Some(participant.as_str()),
                        rank.as_u64(),
                        true,
                        *takeover,
                        write.is_committed(),
                        read.write_rank().as_u64(),
                        read.into_value(),
                    )
                }
                PollOutcome::Follower { owner, rank } => (
                    rank.as_u64(),
                    Some(owner.as_str()),
                    rank.as_u64(),
                    false,
                    false,
                    false,
                    0,
                    None,
                ),
            };
            write_log(
                &txn,
                &log_subspace,
                client_id,
                op_num,
                OP_POLL,
                participant.as_str(),
                prior.rank().as_u64(),
                prior.owner().map(|owner| owner.as_str()),
                generation,
                owner,
                requested_rank,
                result,
                takeover,
                protected_write_committed,
                observed_write_rank,
                observed_value.as_deref(),
                if result { &payload } else { &[] },
            );
            Ok::<_, FdbBindingError>(PollRun {
                observation: poll.into_next_observation(),
                leader_rank: result.then_some(Rank::from(requested_rank)),
                protected_write_committed,
            })
        }
    })
    .await
}

async fn run_resign(
    db: &SimDatabase,
    election: LeaderElection,
    log_subspace: Subspace,
    participant: ParticipantId,
    rank: Rank,
    client_id: i32,
    op_num: u64,
) -> Result<bool, FdbBindingError> {
    db.run(|txn, _maybe_committed| {
        let election = election.clone();
        let log_subspace = log_subspace.clone();
        let participant = participant.clone();
        async move {
            txn.set_option(TransactionOption::AutomaticIdempotency)?;
            let prior = election.state(&txn).await.map_err(FdbBindingError::from)?;
            let resigned = election
                .resign(&txn, &participant, rank)
                .await
                .map_err(FdbBindingError::from)?
                .is_resigned();
            write_log(
                &txn,
                &log_subspace,
                client_id,
                op_num,
                OP_RESIGN,
                participant.as_str(),
                prior.rank().as_u64(),
                prior.owner().map(|owner| owner.as_str()),
                prior.rank().as_u64(),
                if resigned {
                    None
                } else {
                    prior.owner().map(|owner| owner.as_str())
                },
                rank.as_u64(),
                resigned,
                false,
                false,
                0,
                None,
                &[],
            );
            Ok::<_, FdbBindingError>(resigned)
        }
    })
    .await
}

async fn run_observer(
    db: &SimDatabase,
    election: LeaderElection,
    log_subspace: Subspace,
    actor: String,
    client_id: i32,
    op_num: u64,
) -> Result<(), FdbBindingError> {
    db.run(|txn, _maybe_committed| {
        let election = election.clone();
        let log_subspace = log_subspace.clone();
        let actor = actor.clone();
        async move {
            txn.set_option(TransactionOption::AutomaticIdempotency)?;
            let state = election.state(&txn).await.map_err(FdbBindingError::from)?;
            write_log(
                &txn,
                &log_subspace,
                client_id,
                op_num,
                OP_OBSERVE,
                &actor,
                state.rank().as_u64(),
                state.owner().map(|owner| owner.as_str()),
                state.rank().as_u64(),
                state.owner().map(|owner| owner.as_str()),
                state.rank().as_u64(),
                true,
                false,
                false,
                0,
                None,
                &[],
            );
            Ok::<_, FdbBindingError>(())
        }
    })
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_stale_write(
    db: &SimDatabase,
    election: LeaderElection,
    register: RankedRegister,
    log_subspace: Subspace,
    actor: String,
    stale_rank: Rank,
    client_id: i32,
    op_num: u64,
) -> Result<bool, FdbBindingError> {
    let payload = format!("stale:{actor}:{op_num}").into_bytes();
    db.run(|txn, _maybe_committed| {
        let election = election.clone();
        let register = register.clone();
        let log_subspace = log_subspace.clone();
        let actor = actor.clone();
        let payload = payload.clone();
        async move {
            txn.set_option(TransactionOption::AutomaticIdempotency)?;
            let state = election.state(&txn).await.map_err(FdbBindingError::from)?;
            let write = register
                .write(&txn, stale_rank, &payload)
                .await
                .map_err(ranked_register_error)?;
            write_log(
                &txn,
                &log_subspace,
                client_id,
                op_num,
                OP_STALE_WRITE,
                &actor,
                state.rank().as_u64(),
                state.owner().map(|owner| owner.as_str()),
                state.rank().as_u64(),
                state.owner().map(|owner| owner.as_str()),
                stale_rank.as_u64(),
                write.is_committed(),
                false,
                false,
                0,
                None,
                &payload,
            );
            Ok::<_, FdbBindingError>(write.is_committed())
        }
    })
    .await
}

#[allow(clippy::too_many_arguments)]
fn write_log(
    txn: &foundationdb::Transaction,
    log_subspace: &Subspace,
    client_id: i32,
    op_num: u64,
    kind: i64,
    actor: &str,
    prior_generation: u64,
    prior_owner: Option<&str>,
    generation: u64,
    owner: Option<&str>,
    requested_rank: u64,
    result: bool,
    takeover: bool,
    protected_write_committed: bool,
    observed_write_rank: u64,
    observed_value: Option<&[u8]>,
    payload: &[u8],
) {
    let key =
        log_subspace.pack_with_versionstamp(&(Versionstamp::incomplete(0), client_id, op_num));
    let value = pack(&(
        kind,
        actor,
        (
            prior_generation,
            prior_owner.is_some(),
            prior_owner.unwrap_or(""),
        ),
        (generation, owner.is_some(), owner.unwrap_or("")),
        (requested_rank, result, takeover, protected_write_committed),
        (
            observed_write_rank,
            observed_value.is_some(),
            observed_value.unwrap_or(&[]),
        ),
        payload,
    ));
    txn.atomic_op(&key, &value, MutationType::SetVersionstampedKey);
}

fn decode_log_entry(
    log_subspace: &Subspace,
    key: &[u8],
    value: &[u8],
) -> Result<LogEntry, FdbBindingError> {
    let (versionstamp, client_id, op_num): (Versionstamp, i32, u64) = log_subspace
        .unpack(key)
        .map_err(FdbBindingError::PackError)?;
    let (
        kind,
        actor,
        (prior_generation, prior_has_owner, prior_owner),
        (generation, has_owner, owner),
        (requested_rank, result, takeover, protected_write_committed),
        (observed_write_rank, observed_has_value, observed_value),
        payload,
    ): LogWire = unpack(value).map_err(FdbBindingError::PackError)?;
    let prior_owner = optional_owner(prior_has_owner, prior_owner)?;
    let owner = optional_owner(has_owner, owner)?;
    let observed_value = optional_value(observed_has_value, observed_value)?;
    Ok(LogEntry {
        versionstamp,
        client_id,
        op_num,
        kind,
        actor,
        prior_generation,
        prior_owner,
        generation,
        owner,
        requested_rank,
        result,
        takeover,
        protected_write_committed,
        observed_write_rank,
        observed_value,
        payload,
    })
}

type LogWire = (
    i64,
    String,
    (u64, bool, String),
    (u64, bool, String),
    (u64, bool, bool, bool),
    (u64, bool, Vec<u8>),
    Vec<u8>,
);

fn optional_owner(has_owner: bool, owner: String) -> Result<Option<String>, FdbBindingError> {
    match (has_owner, owner.is_empty()) {
        (true, false) => Ok(Some(owner)),
        (false, true) => Ok(None),
        _ => Err(FdbBindingError::new_custom_error(Box::new(LogError(
            "invalid optional owner in operation log".to_owned(),
        )))),
    }
}

fn optional_value(has_value: bool, value: Vec<u8>) -> Result<Option<Vec<u8>>, FdbBindingError> {
    match (has_value, value.is_empty()) {
        (true, _) => Ok(Some(value)),
        (false, true) => Ok(None),
        (false, false) => Err(FdbBindingError::new_custom_error(Box::new(LogError(
            "invalid optional value in operation log".to_owned(),
        )))),
    }
}

fn replay(entries: &[LogEntry], snapshot: &Snapshot) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    let mut generation = 0;
    let mut owner: Option<String> = None;
    let mut protected_rank = 0;
    let mut protected_value = None;
    let mut saw_strict_stale_write = false;

    for entry in entries {
        if !seen.insert((entry.actor.clone(), entry.op_num)) {
            return Err(format!(
                "duplicate logical operation ({}, {}) at {:?}",
                entry.actor, entry.op_num, entry.versionstamp
            ));
        }
        if entry.actor.is_empty() {
            return Err(format!(
                "operation ({}, {}) has an empty actor",
                entry.client_id, entry.op_num
            ));
        }
        if entry.prior_generation != generation || entry.prior_owner != owner {
            return Err(format!(
                "operation ({}, {}) read ({}, {:?}), replay has ({}, {:?})",
                entry.client_id,
                entry.op_num,
                entry.prior_generation,
                entry.prior_owner,
                generation,
                owner
            ));
        }

        match entry.kind {
            OP_POLL => replay_poll(
                entry,
                &mut generation,
                &mut owner,
                &mut protected_rank,
                &mut protected_value,
            )?,
            OP_RESIGN => replay_resign(entry, &mut generation, &mut owner)?,
            OP_OBSERVE => {
                if !entry.result
                    || entry.generation != generation
                    || entry.owner != owner
                    || entry.requested_rank != generation
                    || entry.takeover
                    || entry.protected_write_committed
                    || entry.observed_write_rank != 0
                    || entry.observed_value.is_some()
                    || !entry.payload.is_empty()
                {
                    return Err(format!(
                        "observer ({}, {}) changed or misreported state",
                        entry.client_id, entry.op_num
                    ));
                }
            }
            OP_STALE_WRITE => {
                if entry.result
                    || entry.generation != generation
                    || entry.owner != owner
                    || entry.requested_rank >= generation
                    || entry.takeover
                    || entry.protected_write_committed
                    || entry.observed_write_rank != 0
                    || entry.observed_value.is_some()
                    || entry.payload.is_empty()
                {
                    return Err(format!(
                        "stale write ({}, {}) did not abort at generation {}",
                        entry.client_id, entry.op_num, generation
                    ));
                }
                saw_strict_stale_write = true;
            }
            other => return Err(format!("unknown operation kind {other}")),
        }
    }

    if snapshot.generation != generation || snapshot.owner != owner {
        return Err(format!(
            "final election state ({}, {:?}) differs from replay ({}, {:?})",
            snapshot.generation, snapshot.owner, generation, owner
        ));
    }
    if snapshot.protected_rank != protected_rank || snapshot.protected_value != protected_value {
        return Err("final ranked-register state differs from replay".to_owned());
    }
    if protected_value.is_none() {
        return Err("no leader poll committed a fenced protected write".to_owned());
    }
    if !saw_strict_stale_write {
        return Err("no strictly stale ranked-register write was exercised".to_owned());
    }
    Ok(())
}

fn replay_poll(
    entry: &LogEntry,
    generation: &mut u64,
    owner: &mut Option<String>,
    protected_rank: &mut u64,
    protected_value: &mut Option<Vec<u8>>,
) -> Result<(), String> {
    if entry.result {
        let incumbent = owner.as_deref() == Some(entry.actor.as_str());
        let unowned = owner.is_none();
        let takeover = !incumbent && !unowned;
        let expected_generation = generation.checked_add(1).ok_or("generation overflow")?;
        let expected_owner = entry.actor.as_str();
        let expected_payload = protected_payload(expected_owner, entry.op_num);
        if entry.actor.is_empty()
            || entry.generation != expected_generation
            || entry.owner.as_deref() != Some(expected_owner)
            || entry.requested_rank != entry.generation
            || entry.takeover != takeover
            || !entry.protected_write_committed
            || entry.observed_write_rank != *protected_rank
            || entry.observed_value != *protected_value
            || entry.payload != expected_payload
        {
            return Err(format!(
                "leader poll ({}, {}) mismatch: generation actual={} expected={}; owner actual={:?} expected={:?}; requested_rank actual={} expected={}; takeover actual={} expected={}; protected_write_committed actual={} expected=true; observed_write_rank actual={} expected={}; observed_value actual={:?} expected={:?}; payload actual={:?} expected={:?}",
                entry.client_id,
                entry.op_num,
                entry.generation,
                expected_generation,
                entry.owner,
                expected_owner,
                entry.requested_rank,
                entry.generation,
                entry.takeover,
                takeover,
                entry.protected_write_committed,
                entry.observed_write_rank,
                protected_rank,
                entry.observed_value,
                protected_value,
                entry.payload,
                expected_payload,
            ));
        }
        *generation = entry.generation;
        *owner = entry.owner.clone();
        *protected_rank = entry.generation;
        *protected_value = Some(entry.payload.clone());
    } else if owner.is_some()
        && entry.generation == *generation
        && entry.owner == *owner
        && entry.requested_rank == *generation
        && !entry.takeover
        && !entry.protected_write_committed
        && entry.observed_write_rank == 0
        && entry.observed_value.is_none()
        && entry.payload.is_empty()
    {
    } else {
        return Err(format!(
            "follower poll ({}, {}) does not report the replay state",
            entry.client_id, entry.op_num
        ));
    }
    Ok(())
}

fn replay_resign(
    entry: &LogEntry,
    generation: &mut u64,
    owner: &mut Option<String>,
) -> Result<(), String> {
    let matches =
        owner.as_deref() == Some(entry.actor.as_str()) && entry.requested_rank == *generation;
    if entry.result != matches
        || entry.generation != *generation
        || entry.takeover
        || entry.protected_write_committed
        || entry.observed_write_rank != 0
        || entry.observed_value.is_some()
        || !entry.payload.is_empty()
    {
        return Err(format!(
            "resign ({}, {}) does not match current owner and generation",
            entry.client_id, entry.op_num
        ));
    }
    let expected_owner = if matches { None } else { owner.clone() };
    if entry.owner != expected_owner {
        return Err(format!(
            "resign ({}, {}) reported an invalid resulting owner",
            entry.client_id, entry.op_num
        ));
    }
    *owner = expected_owner;
    Ok(())
}

fn simulated_now(context: &WorkloadContext) -> Duration {
    Duration::from_secs_f64(context.now().max(0.0))
}

fn protected_payload(actor: &str, op_num: u64) -> Vec<u8> {
    format!("leader:{actor}:{op_num}").into_bytes()
}

fn ranked_register_error(error: RankedRegisterError) -> FdbBindingError {
    FdbBindingError::new_custom_error(Box::new(error))
}

#[derive(Debug)]
struct LogError(String);

impl std::fmt::Display for LogError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for LogError {}
