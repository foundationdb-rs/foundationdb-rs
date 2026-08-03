// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Deterministic workload and independent commit-order oracle for leader leases.

use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

use foundationdb::{
    FdbBindingError, RangeOption,
    options::{MutationType, TransactionOption},
    recipes::{
        leader_election::{
            ElectionState, LeaderElection, Leadership, LocalState, ParticipantId, PollOutcome,
            PollTransition,
        },
        ranked_register::{Rank, RankedRegister, RankedRegisterError},
    },
    tuple::{Subspace, Versionstamp, pack, unpack},
};
use foundationdb_simulation::{
    Metric, Metrics, RustWorkload, Severity, SimDatabase, SingleRustWorkload, WorkloadContext,
    details,
};
use futures::TryStreamExt;

use super::types::{
    DurableState, LOCAL_LEADERSHIP, LOCAL_OBSERVATION, LOCAL_UNKNOWN, LocalInput, LogEntry,
    OP_OBSERVE, OP_POLL, OP_RESIGN, OP_STALE_WRITE, Snapshot, TRANSITION_ACQUIRED,
    TRANSITION_FOLLOWED, TRANSITION_NONE, TRANSITION_REACQUIRED, TRANSITION_RENEWED,
    TRANSITION_TOOK_OVER,
};

const OPTIONAL_RESIGN: u32 = 1;
const OPTIONAL_OBSERVER: u32 = 2;
const OPTIONAL_PAUSE: u32 = 4;
const OPTIONAL_DELAYED_ADOPTION: u32 = 8;

#[derive(Clone, Copy)]
enum SwarmProfile {
    Standard,
    Contention,
    Suspicion,
}

impl SwarmProfile {
    fn from_random(random: u32) -> Self {
        match random % 3 {
            0 => Self::Standard,
            1 => Self::Contention,
            _ => Self::Suspicion,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Contention => "contention",
            Self::Suspicion => "suspicion",
        }
    }

    fn metric_value(self) -> f64 {
        match self {
            Self::Standard => 0.0,
            Self::Contention => 1.0,
            Self::Suspicion => 2.0,
        }
    }

    fn operation_count(self) -> usize {
        match self {
            Self::Standard => 40,
            Self::Contention => 160,
            Self::Suspicion => 80,
        }
    }

    fn lease_base_secs(self) -> u64 {
        match self {
            Self::Standard => 2,
            Self::Contention => 1,
            Self::Suspicion => 3,
        }
    }
}

pub struct LeaderElectionWorkload {
    context: WorkloadContext,
    client_id: i32,
    profile: SwarmProfile,
    operation_count: usize,
    lease_base_secs: u64,
    lease_duration: Duration,
    election_subspace: Subspace,
    register_subspace: Subspace,
    log_subspace: Subspace,
    participant: ParticipantId,
    incarnation: u64,
    local_state: LocalState,
    last_leadership: Option<Leadership>,
    stale_leadership: Option<Leadership>,
    stale_rank: Option<Rank>,
    zero_stale_write_completed: bool,
    stale_actions_completed: bool,
    expiry_pause_completed: bool,
    normal_weight: u32,
    adversarial_weight: u32,
    op_num: u64,
    swarm: u32,
    poll_count: u64,
    leader_count: u64,
    run_errors: u64,
    delayed_adoption_count: u64,
    delayed_adoption_sub_lease_count: u64,
    delayed_adoption_exact_lease_count: u64,
    delayed_adoption_over_lease_count: u64,
}

impl SingleRustWorkload for LeaderElectionWorkload {
    fn new(_name: String, context: WorkloadContext) -> Self {
        let client_id = context.client_id();
        let profile = SwarmProfile::from_random(context.rnd());
        let operation_count = context
            .get_option("operationCount")
            .unwrap_or(profile.operation_count());
        let lease_base_secs = context
            .get_option("suspicionSecs")
            .unwrap_or(profile.lease_base_secs());
        let lease_duration = Duration::from_secs(lease_base_secs + u64::from(context.rnd() % 3));
        debug_assert!(ParticipantId::new("").is_err());
        let process_id = context.get_process_id();
        let participant_text = match context.rnd() % 4 {
            0 => format!("participant-{client_id}-{process_id}"),
            1 => format!("participant\0{client_id}-{process_id}"),
            2 => format!("participant-λ-{client_id}-{process_id}"),
            _ => format!("participant-{client_id}-{process_id}-{}", "x".repeat(128)),
        };
        let participant =
            ParticipantId::new(participant_text).expect("generated participant IDs are non-empty");

        Self {
            client_id,
            profile,
            operation_count,
            lease_base_secs,
            lease_duration,
            election_subspace: Subspace::all().subspace(&("leader-lease",)),
            register_subspace: Subspace::all().subspace(&("leader-lease-register",)),
            log_subspace: Subspace::all().subspace(&("leader-lease-log",)),
            participant,
            incarnation: 0,
            local_state: LocalState::unknown(),
            last_leadership: None,
            stale_leadership: None,
            stale_rank: None,
            zero_stale_write_completed: false,
            stale_actions_completed: false,
            expiry_pause_completed: false,
            normal_weight: 50 + context.rnd() % 21,
            adversarial_weight: 20 + context.rnd() % 21,
            op_num: 0,
            swarm: context.rnd(),
            poll_count: 0,
            leader_count: 0,
            run_errors: 0,
            delayed_adoption_count: 0,
            delayed_adoption_sub_lease_count: 0,
            delayed_adoption_exact_lease_count: 0,
            delayed_adoption_over_lease_count: 0,
            context,
        }
    }
}

impl RustWorkload for LeaderElectionWorkload {
    async fn setup(&mut self, _db: SimDatabase) {
        self.context.trace(
            Severity::Info,
            "LeaderLeaseSetup",
            details![
                "Layer" => "Rust",
                "Client" => self.client_id,
                "Profile" => self.profile.name(),
                "OperationCount" => self.operation_count,
                "LeaseBaseSecs" => self.lease_base_secs,
                "LeaseSecs" => self.lease_duration.as_secs(),
                "Protocol" => "poll-lease-fenced-ranked-register"
            ],
        );
    }

    async fn start(&mut self, db: SimDatabase) {
        let register = RankedRegister::new(self.register_subspace.clone());

        for round in 0..self.operation_count {
            let operation_weight = self.context.rnd() % 100;
            let duration_change = operation_weight
                >= self.normal_weight + 4 * self.adversarial_weight / 5
                && operation_weight < self.normal_weight + self.adversarial_weight;
            let election = LeaderElection::new(
                self.election_subspace.clone(),
                if duration_change {
                    self.lease_duration_for_round(round)
                } else {
                    self.lease_duration
                },
            )
            .expect("generated lease duration is non-zero");
            let run_normal_poll =
                round == 0 || operation_weight < self.normal_weight || duration_change;
            let mut forced_expiry_pause = false;

            if run_normal_poll {
                self.poll_count += 1;
                let op_num = self.next_op_num();
                let adoption_delay = self.random_adoption_delay();
                let poll = run_poll(
                    &db,
                    election.clone(),
                    register.clone(),
                    self.log_subspace.clone(),
                    self.participant.clone(),
                    self.local_state.clone(),
                    true,
                    &self.context,
                    self.client_id,
                    self.incarnation,
                    op_num,
                    adoption_delay,
                )
                .await;

                let mut exercise_zero_stale_write = false;
                match poll {
                    Ok(poll) => {
                        if let Some(delay) = poll.adoption_delay {
                            self.record_delayed_adoption(delay);
                        }
                        self.local_state = poll.next_state;
                        if let Some(leadership) = poll.leadership {
                            if let Some(previous) = self
                                .last_leadership
                                .as_ref()
                                .filter(|previous| previous.rank() < leadership.rank())
                                .cloned()
                            {
                                self.stale_rank = Some(previous.rank());
                                self.stale_leadership = Some(previous);
                            }
                            self.last_leadership = Some(leadership);
                            self.leader_count += 1;
                            forced_expiry_pause = !self.expiry_pause_completed;
                            exercise_zero_stale_write = !self.zero_stale_write_completed;
                        }
                    }
                    Err(error) => {
                        self.run_diagnostic("PollRunFailed", op_num, error);
                        self.replace_incarnation("poll-error");
                    }
                }

                if exercise_zero_stale_write {
                    let stale_write_op = self.next_op_num();
                    match run_stale_write(
                        &db,
                        election.clone(),
                        register.clone(),
                        self.log_subspace.clone(),
                        self.participant.as_str().to_owned(),
                        Rank::ZERO,
                        simulated_now(&self.context),
                        self.client_id,
                        self.incarnation,
                        stale_write_op,
                    )
                    .await
                    {
                        Ok(true) => self.protocol_error(
                            "ZeroRankStaleWriteCommitted",
                            stale_write_op,
                            "a rank-zero ranked-register write committed after leadership began",
                        ),
                        Ok(false) => self.zero_stale_write_completed = true,
                        Err(error) => {
                            self.run_diagnostic("ZeroRankStaleWriteFailed", stale_write_op, error)
                        }
                    }
                }
            }

            if self.swarm & OPTIONAL_OBSERVER != 0
                && operation_weight >= self.normal_weight
                && operation_weight < self.normal_weight + self.adversarial_weight / 3
            {
                let op_num = self.next_op_num();
                if let Err(error) = run_observer(
                    &db,
                    election.clone(),
                    self.log_subspace.clone(),
                    self.participant.as_str().to_owned(),
                    self.lease_duration,
                    simulated_now(&self.context),
                    self.client_id,
                    self.incarnation,
                    op_num,
                )
                .await
                {
                    self.run_diagnostic("ObserverRunFailed", op_num, error);
                }
            }

            if self.swarm & OPTIONAL_RESIGN != 0
                && operation_weight >= self.normal_weight + self.adversarial_weight / 3
                && operation_weight < self.normal_weight + 2 * self.adversarial_weight / 3
            {
                if let Some(leadership) = self.local_state.leadership().cloned() {
                    let op_num = self.next_op_num();
                    match run_resign(
                        &db,
                        election.clone(),
                        self.log_subspace.clone(),
                        leadership,
                        simulated_now(&self.context),
                        self.client_id,
                        self.incarnation,
                        true,
                        op_num,
                    )
                    .await
                    {
                        Ok(true) => {
                            self.local_state = LocalState::unknown();
                            self.force_foreign_takeover(
                                &db,
                                &register,
                                election.lease_duration(),
                                (self.swarm & OPTIONAL_DELAYED_ADOPTION != 0)
                                    .then_some(AdoptionDelay::Longer),
                            )
                            .await;
                        }
                        Ok(false) => {}
                        Err(error) => {
                            self.run_diagnostic("ResignRunFailed", op_num, error);
                            self.replace_incarnation("resign-error");
                        }
                    }
                }
            }

            if operation_weight >= self.normal_weight + 2 * self.adversarial_weight / 3
                && operation_weight < self.normal_weight + 4 * self.adversarial_weight / 5
                && !self.stale_actions_completed
            {
                if let (Some(stale_leadership), Some(stale_rank)) =
                    (self.stale_leadership.clone(), self.stale_rank)
                {
                    self.exercise_stale_tokens(&db, &register, stale_leadership, stale_rank)
                        .await;
                }
            }

            if operation_weight >= self.normal_weight + self.adversarial_weight {
                self.replace_incarnation("dropped");
                self.context.trace(
                    Severity::Info,
                    "LeaderLeaseLostLocalState",
                    details!["Client" => self.client_id, "Round" => round],
                );
            }

            if round % 3 == 0 {
                if let Err(error) =
                    continuous_snapshot_check(&db, election.clone(), register.clone()).await
                {
                    if error.get_fdb_error().is_some() {
                        self.run_diagnostic("ContinuousSnapshotFailed", self.op_num, error);
                    } else {
                        self.protocol_error(
                            "ContinuousSnapshotInvariantFailed",
                            self.op_num,
                            "one-transaction public snapshot violated structural invariants",
                        );
                    }
                }
            }

            let boundary_pause = forced_expiry_pause
                || (self.swarm & OPTIONAL_PAUSE != 0
                    && operation_weight >= self.normal_weight + 4 * self.adversarial_weight / 5
                    && operation_weight < self.normal_weight + self.adversarial_weight);
            if boundary_pause {
                if forced_expiry_pause {
                    self.expiry_pause_completed = true;
                }
                let target = match &self.local_state {
                    LocalState::Observation(observation) => observation
                        .first_observed_at()
                        .saturating_add(observation.lease_duration())
                        .saturating_add(Duration::from_millis(1)),
                    LocalState::Leadership(leadership) => leadership
                        .last_renewed_at()
                        .saturating_add(leadership.lease_duration())
                        .saturating_add(Duration::from_millis(1)),
                    LocalState::Unknown => {
                        simulated_now(&self.context).saturating_add(Duration::from_millis(1))
                    }
                };
                delay_until(&self.context, target).await;
                continue;
            }
            let delay = Duration::from_millis(1);
            if let Err(error) = self.context.delay(delay).await {
                self.context.trace(
                    Severity::Warn,
                    "SimulationDelayFailed",
                    details!["Client" => self.client_id, "Error" => format!("{error:?}")],
                );
            }
        }

        self.complete_witnesses(db, register).await;
    }

    async fn check(&mut self, db: SimDatabase) {
        if self.client_id != 0 {
            return;
        }

        let log_subspace = self.log_subspace.clone();
        let election = LeaderElection::new(self.election_subspace.clone(), self.lease_duration)
            .expect("configured lease duration is non-zero");
        let register = RankedRegister::new(self.register_subspace.clone());
        let result = db
            .run(|txn, _maybe_committed| {
                let log_subspace = log_subspace.clone();
                let election = election.clone();
                let register = register.clone();
                async move {
                    txn.set_option(TransactionOption::AutomaticIdempotency)?;
                    let read_version = txn.get_read_version().await?;
                    let key_values: Vec<_> = txn
                        .get_ranges_keyvalues(RangeOption::from(log_subspace.range()), false)
                        .try_collect()
                        .await
                        .map_err(FdbBindingError::from)?;
                    let mut entries = Vec::with_capacity(key_values.len());
                    for key_value in key_values {
                        entries.push(decode_log_entry(
                            &log_subspace,
                            key_value.key(),
                            key_value.value(),
                        )?);
                    }
                    let election = durable_from_public(
                        election.state(&txn).await.map_err(FdbBindingError::from)?,
                    );
                    let protected = register
                        .read(&txn, Rank::ZERO)
                        .await
                        .map_err(ranked_register_error)?;
                    Ok::<_, FdbBindingError>((
                        read_version,
                        entries,
                        Snapshot {
                            election,
                            protected_rank: protected.write_rank().as_u64(),
                            protected_value: protected.into_value(),
                        },
                    ))
                }
            })
            .await;

        match result {
            Ok((read_version, entries, snapshot)) => match replay(&entries, &snapshot) {
                Ok(coverage) if coverage.missing.contains(&"delayed_observation_adoption") => {
                    self.context.trace(
                        Severity::Error,
                        "LeaderLeaseDelayedAdoptionMissing",
                        details![
                            "ReadVersion" => read_version,
                            "Entries" => entries.len(),
                            "WitnessesObserved" => coverage.observed,
                            "MissingWitnesses" => coverage.missing.join(", "),
                        ],
                    )
                }
                Ok(coverage) => self.context.trace(
                    Severity::Info,
                    "LeaderLeaseCheckPassed",
                    details![
                        "ReadVersion" => read_version,
                        "Entries" => entries.len(),
                        "WitnessesObserved" => coverage.observed,
                        "MissingWitnesses" => coverage.missing.join(", "),
                    ],
                ),
                Err(error) => self.context.trace(
                    Severity::Error,
                    "LeaderLeaseInvariantFailed",
                    details!["ReadVersion" => read_version, "Error" => error],
                ),
            },
            Err(error) => self.context.trace(
                Severity::Error,
                "LeaderLeaseCheckReadFailed",
                details!["Error" => format!("{error:?}")],
            ),
        }
    }

    fn get_metrics(&self, mut out: Metrics) {
        out.extend([
            Metric::val("swarm_profile", self.profile.metric_value()),
            Metric::val("operation_count", self.operation_count as f64),
            Metric::val("lease_base_secs", self.lease_base_secs as f64),
            Metric::val("poll_count", self.poll_count as f64),
            Metric::val("leader_count", self.leader_count as f64),
            Metric::val("run_errors", self.run_errors as f64),
            Metric::val("delayed_adoption_count", self.delayed_adoption_count as f64),
            Metric::val(
                "delayed_adoption_sub_lease_count",
                self.delayed_adoption_sub_lease_count as f64,
            ),
            Metric::val(
                "delayed_adoption_exact_lease_count",
                self.delayed_adoption_exact_lease_count as f64,
            ),
            Metric::val(
                "delayed_adoption_over_lease_count",
                self.delayed_adoption_over_lease_count as f64,
            ),
        ]);
    }

    fn get_check_timeout(&self) -> f64 {
        5000.0
    }
}

impl LeaderElectionWorkload {
    async fn force_foreign_takeover(
        &mut self,
        db: &SimDatabase,
        register: &RankedRegister,
        follower_duration: Duration,
        adoption_delay: Option<AdoptionDelay>,
    ) {
        let foreign = ParticipantId::new(format!(
            "foreign\0λ-{}-{}-{}",
            self.client_id,
            self.context.get_process_id(),
            self.incarnation,
        ))
        .expect("generated foreign participant is non-empty");
        let foreign_duration = follower_duration + Duration::from_secs(2);
        let foreign_election =
            LeaderElection::new(self.election_subspace.clone(), foreign_duration)
                .expect("foreign lease duration is non-zero");
        let foreign_op = self.next_op_num();
        let foreign_poll = run_poll(
            db,
            foreign_election,
            register.clone(),
            self.log_subspace.clone(),
            foreign,
            LocalState::unknown(),
            false,
            &self.context,
            self.client_id,
            self.incarnation,
            foreign_op,
            None,
        )
        .await;
        let Ok(foreign_poll) = foreign_poll else {
            return;
        };
        let Some(predecessor) = foreign_poll.leadership else {
            return;
        };

        self.local_state = LocalState::unknown();
        let _ = self
            .poll_once(db, register, follower_duration, adoption_delay)
            .await;
        let Some(observation) = self.local_state.observation().cloned() else {
            return;
        };
        if observation.owner().as_str() == self.participant.as_str()
            || observation.lease_duration() != predecessor.lease_duration()
        {
            return;
        }
        let before = observation
            .first_observed_at()
            .saturating_add(observation.lease_duration())
            .saturating_sub(Duration::from_millis(1));
        delay_until(&self.context, before).await;
        let _ = self.poll_once(db, register, follower_duration, None).await;
        let exact = observation
            .first_observed_at()
            .saturating_add(observation.lease_duration());
        delay_until(&self.context, exact).await;
        let takeover = self.poll_once(db, register, follower_duration, None).await;
        if takeover.is_some_and(|leadership| leadership.rank() > predecessor.rank()) {
            self.exercise_stale_tokens(db, register, predecessor.clone(), predecessor.rank())
                .await;
        }
    }
}

impl LeaderElectionWorkload {
    fn lease_duration_for_round(&self, round: usize) -> Duration {
        self.lease_duration
            + Duration::from_secs(u64::from((self.swarm.wrapping_add(round as u32)) % 3))
    }

    fn next_op_num(&mut self) -> u64 {
        let op_num = self.op_num;
        self.op_num += 1;
        op_num
    }

    fn random_adoption_delay(&self) -> Option<AdoptionDelay> {
        (self.swarm & OPTIONAL_DELAYED_ADOPTION != 0)
            .then(|| AdoptionDelay::from_random(self.context.rnd()))
    }

    fn record_delayed_adoption(&mut self, delay: AdoptionDelay) {
        self.delayed_adoption_count += 1;
        match delay {
            AdoptionDelay::Shorter => self.delayed_adoption_sub_lease_count += 1,
            AdoptionDelay::Exact => self.delayed_adoption_exact_lease_count += 1,
            AdoptionDelay::Longer => self.delayed_adoption_over_lease_count += 1,
        }
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
                "MaybeCommitted" => error.get_fdb_error().is_some_and(|error| error.is_maybe_committed())
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

    fn replace_incarnation(&mut self, label: &str) {
        self.incarnation += 1;
        self.participant = ParticipantId::new(format!(
            "{label}\0λ-{}-{}-{}",
            self.client_id,
            self.context.get_process_id(),
            self.incarnation,
        ))
        .expect("generated incarnation ID is non-empty");
        self.local_state = LocalState::unknown();
        self.last_leadership = None;
        self.stale_leadership = None;
        self.stale_rank = None;
    }

    async fn poll_once(
        &mut self,
        db: &SimDatabase,
        register: &RankedRegister,
        lease_duration: Duration,
        adoption_delay: Option<AdoptionDelay>,
    ) -> Option<Leadership> {
        let election = LeaderElection::new(self.election_subspace.clone(), lease_duration)
            .expect("completion lease duration is non-zero");
        let op_num = self.next_op_num();
        let result = run_poll(
            db,
            election,
            register.clone(),
            self.log_subspace.clone(),
            self.participant.clone(),
            self.local_state.clone(),
            true,
            &self.context,
            self.client_id,
            self.incarnation,
            op_num,
            adoption_delay,
        )
        .await;
        match result {
            Ok(poll) => {
                if let Some(delay) = poll.adoption_delay {
                    self.record_delayed_adoption(delay);
                }
                self.local_state = poll.next_state;
                if let Some(leadership) = poll.leadership {
                    if let Some(previous) = self
                        .last_leadership
                        .as_ref()
                        .filter(|previous| previous.rank() < leadership.rank())
                        .cloned()
                    {
                        self.stale_leadership = Some(previous.clone());
                        self.stale_rank = Some(previous.rank());
                    }
                    self.last_leadership = Some(leadership.clone());
                    Some(leadership)
                } else {
                    None
                }
            }
            Err(error) => {
                self.run_diagnostic("CompletionPollFailed", op_num, error);
                self.replace_incarnation("completion-poll-error");
                None
            }
        }
    }

    async fn exercise_stale_tokens(
        &mut self,
        db: &SimDatabase,
        register: &RankedRegister,
        stale_leadership: Leadership,
        stale_rank: Rank,
    ) {
        let election = LeaderElection::new(self.election_subspace.clone(), self.lease_duration)
            .expect("configured lease duration is non-zero");
        let stale_poll_op = self.next_op_num();
        match run_poll(
            db,
            election.clone(),
            register.clone(),
            self.log_subspace.clone(),
            self.participant.clone(),
            LocalState::Leadership(stale_leadership.clone()),
            false,
            &self.context,
            self.client_id,
            self.incarnation,
            stale_poll_op,
            None,
        )
        .await
        {
            Ok(poll) if poll.leadership.is_some() => self.protocol_error(
                "StaleRenewSucceeded",
                stale_poll_op,
                "a stale leadership token changed durable state",
            ),
            Ok(_) => {}
            Err(error) => self.run_diagnostic("StaleRenewRunFailed", stale_poll_op, error),
        }

        delay_until(
            &self.context,
            stale_leadership
                .last_renewed_at()
                .saturating_add(stale_leadership.lease_duration())
                .saturating_add(Duration::from_millis(1)),
        )
        .await;

        let stale_resign_op = self.next_op_num();
        match run_resign(
            db,
            election.clone(),
            self.log_subspace.clone(),
            stale_leadership,
            simulated_now(&self.context),
            self.client_id,
            self.incarnation,
            false,
            stale_resign_op,
        )
        .await
        {
            Ok(true) => self.protocol_error(
                "StaleResignSucceeded",
                stale_resign_op,
                "a stale leadership token released durable state",
            ),
            Ok(false) => {}
            Err(error) => self.run_diagnostic("StaleResignRunFailed", stale_resign_op, error),
        }

        let stale_write_op = self.next_op_num();
        match run_stale_write(
            db,
            election,
            register.clone(),
            self.log_subspace.clone(),
            self.participant.as_str().to_owned(),
            stale_rank,
            simulated_now(&self.context),
            self.client_id,
            self.incarnation,
            stale_write_op,
        )
        .await
        {
            Ok(true) => self.protocol_error(
                "StaleWriteCommitted",
                stale_write_op,
                "a strictly stale ranked-register write committed",
            ),
            Ok(false) => self.stale_actions_completed = true,
            Err(error) => self.run_diagnostic("StaleWriteRunFailed", stale_write_op, error),
        }
    }

    async fn complete_witnesses(&mut self, db: SimDatabase, register: RankedRegister) {
        self.replace_incarnation("replacement");

        for phase in 0..3 {
            let lease_duration = self.lease_duration_for_round(self.operation_count + phase);
            let adoption_delay = AdoptionDelay::for_completion_phase(phase);
            if let Some(first) = self
                .poll_once(&db, &register, lease_duration, Some(adoption_delay))
                .await
            {
                let renewal_duration =
                    self.lease_duration_for_round(self.operation_count + phase + 1);
                if let Some(renewed) = self
                    .poll_once(&db, &register, renewal_duration, Some(adoption_delay))
                    .await
                {
                    if first.rank() < renewed.rank() {
                        self.exercise_stale_tokens(&db, &register, first.clone(), first.rank())
                            .await;
                    }
                    let resignation_election =
                        LeaderElection::new(self.election_subspace.clone(), renewal_duration)
                            .expect("completion lease duration is non-zero");
                    let resign_op = self.next_op_num();
                    match run_resign(
                        &db,
                        resignation_election,
                        self.log_subspace.clone(),
                        renewed,
                        simulated_now(&self.context),
                        self.client_id,
                        self.incarnation,
                        true,
                        resign_op,
                    )
                    .await
                    {
                        Ok(true) => {
                            self.local_state = LocalState::unknown();
                            self.force_foreign_takeover(
                                &db,
                                &register,
                                renewal_duration,
                                Some(adoption_delay),
                            )
                            .await;
                        }
                        Ok(false) => self.protocol_error(
                            "ExactResignRejected",
                            resign_op,
                            "fresh leadership token did not resign",
                        ),
                        Err(error) => {
                            self.run_diagnostic("ExactResignFailed", resign_op, error);
                            self.replace_incarnation("exact-resign-error");
                        }
                    }
                }
                self.replace_incarnation("completion-reset");
                let _ = self
                    .poll_once(&db, &register, renewal_duration, Some(adoption_delay))
                    .await;
            }

            if let Some(observation) = self.local_state.observation().cloned() {
                let before_expiry = observation
                    .first_observed_at()
                    .saturating_add(observation.lease_duration())
                    .saturating_sub(Duration::from_millis(1));
                let _ = delay_until(&self.context, before_expiry).await;
                let _ = self
                    .poll_once(&db, &register, lease_duration, Some(adoption_delay))
                    .await;
                let exact_expiry = observation
                    .first_observed_at()
                    .saturating_add(observation.lease_duration());
                let _ = delay_until(&self.context, exact_expiry).await;
                let _ = self
                    .poll_once(&db, &register, lease_duration, Some(adoption_delay))
                    .await;
            }

            if let Some(leadership) = self.local_state.leadership().cloned() {
                let after_expiry = leadership
                    .last_renewed_at()
                    .saturating_add(leadership.lease_duration())
                    .saturating_add(Duration::from_millis(1));
                let _ = delay_until(&self.context, after_expiry).await;
                let _ = self
                    .poll_once(&db, &register, lease_duration, Some(adoption_delay))
                    .await;
            }
        }
    }
}

struct PollRun {
    next_state: LocalState,
    leadership: Option<Leadership>,
    adoption_delay: Option<AdoptionDelay>,
}

#[derive(Clone, Copy)]
enum AdoptionDelay {
    Shorter,
    Exact,
    Longer,
}

impl AdoptionDelay {
    fn from_random(random: u32) -> Self {
        match random % 3 {
            0 => Self::Shorter,
            1 => Self::Exact,
            _ => Self::Longer,
        }
    }

    fn duration(self, lease_duration: Duration) -> Duration {
        match self {
            Self::Shorter => lease_duration / 2,
            Self::Exact => lease_duration,
            Self::Longer => lease_duration.saturating_add(Duration::from_millis(1)),
        }
    }

    fn for_completion_phase(phase: usize) -> Self {
        [Self::Shorter, Self::Exact, Self::Longer][phase]
    }
}

#[derive(Clone)]
enum LocalExpectation {
    Exact(LocalInput),
    AdoptedObservation {
        owner: String,
        rank: u64,
        lease_duration: Duration,
        not_before: Duration,
        planned_adoption_delay: Option<Duration>,
    },
}

struct CoverageReport {
    observed: usize,
    missing: Vec<&'static str>,
}

#[derive(Default)]
struct ReplayWitnesses {
    saw_expiry: bool,
    saw_foreign_takeover: bool,
    last_takeover_rank: Option<u64>,
    saw_before_expiry: bool,
    saw_exact_expiry: bool,
    saw_after_expiry: bool,
    saw_stale_renew: bool,
    saw_stale_renew_after_advance: bool,
    saw_stale_resign: bool,
    saw_delayed_stale_resign: bool,
    saw_stale_resign_after_takeover: bool,
    saw_exact_resign: bool,
    saw_stale_write: bool,
    saw_zero_rank_stale_write: bool,
    saw_post_advance_stale_write: bool,
    renewal_with_new_duration_rank: Option<u64>,
    saw_follower_duration_reset: bool,
    saw_delayed_observation_adoption: bool,
}

async fn continuous_snapshot_check(
    db: &SimDatabase,
    election: LeaderElection,
    register: RankedRegister,
) -> Result<(), FdbBindingError> {
    let snapshot = db
        .run(|txn, _maybe_committed| {
            let election = election.clone();
            let register = register.clone();
            async move {
                txn.set_option(TransactionOption::AutomaticIdempotency)?;
                let election =
                    durable_from_public(election.state(&txn).await.map_err(FdbBindingError::from)?);
                let protected = register
                    .read(&txn, Rank::ZERO)
                    .await
                    .map_err(ranked_register_error)?;
                Ok::<_, FdbBindingError>((
                    election,
                    protected.write_rank().as_u64(),
                    protected.into_value(),
                ))
            }
        })
        .await?;
    let (election, protected_rank, protected_value) = snapshot;
    let valid_election = match election.rank {
        0 => election.owner.is_none() && election.lease_duration.is_none(),
        _ => election
            .lease_duration
            .is_some_and(|duration| !duration.is_zero()),
    };
    let valid_register =
        protected_rank <= election.rank && (protected_rank == 0) == protected_value.is_none();
    if valid_election && valid_register {
        Ok(())
    } else {
        Err(log_error("invalid public election/register snapshot"))
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_poll(
    db: &SimDatabase,
    election: LeaderElection,
    register: RankedRegister,
    log_subspace: Subspace,
    participant: ParticipantId,
    local_state: LocalState,
    tracks_local_state: bool,
    context: &WorkloadContext,
    client_id: i32,
    incarnation: u64,
    op_num: u64,
    adoption_delay: Option<AdoptionDelay>,
) -> Result<PollRun, FdbBindingError> {
    let payload = protected_payload(participant.as_str(), op_num);
    let configured_lease_duration = election.lease_duration();
    let (poll, attempt_started_at, planned_adoption_delay) = db
        .run(|txn, _maybe_committed| {
            let election = election.clone();
            let register = register.clone();
            let log_subspace = log_subspace.clone();
            let participant = participant.clone();
            let local_state = local_state.clone();
            let payload = payload.clone();
            let context = context.clone();
            async move {
                let attempt_started_at = simulated_now(&context);
                txn.set_option(TransactionOption::AutomaticIdempotency)?;
                let prior =
                    durable_from_public(election.state(&txn).await.map_err(FdbBindingError::from)?);
                let local_token = local_input(&local_state);
                let poll = election
                    .poll(&txn, &participant, &local_state, attempt_started_at)
                    .await
                    .map_err(FdbBindingError::from)?;
                let transition = transition_code(poll.outcome().transition());
                let leader = matches!(poll.outcome(), PollOutcome::Leader { .. });
                let (requested_write_rank, observed_write_rank, observed_value, write_committed) =
                    if let PollOutcome::Leader { rank, .. } = poll.outcome() {
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
                            read.write_rank().as_u64(),
                            read.into_value(),
                            write.is_committed(),
                        )
                    } else {
                        (0, 0, None, false)
                    };
                let current =
                    durable_from_public(election.state(&txn).await.map_err(FdbBindingError::from)?);
                let planned_adoption_delay =
                    planned_follower_adoption_delay(&poll, &local_state, adoption_delay);
                write_log(
                    &txn,
                    &log_subspace,
                    client_id,
                    incarnation,
                    op_num,
                    OP_POLL,
                    participant.as_str(),
                    &prior,
                    &current,
                    &local_token,
                    tracks_local_state,
                    attempt_started_at,
                    configured_lease_duration,
                    planned_adoption_delay,
                    transition,
                    leader,
                    requested_write_rank,
                    observed_write_rank,
                    observed_value.as_deref(),
                    write_committed,
                    if leader { &payload } else { &[] },
                );
                Ok::<_, FdbBindingError>((poll, attempt_started_at, planned_adoption_delay))
            }
        })
        .await?;
    let adoption_delay = match (adoption_delay, planned_adoption_delay) {
        (Some(delay), Some(duration)) => match context.delay(duration).await {
            Ok(()) => Some(delay),
            Err(error) => return Err(FdbBindingError::from(error)),
        },
        _ => None,
    };
    let adopted_at =
        simulated_now(context).max(attempt_started_at.saturating_add(Duration::from_nanos(1)));
    let next_state = poll.into_next_state(adopted_at);
    Ok(PollRun {
        leadership: next_state.leadership().cloned(),
        next_state,
        adoption_delay,
    })
}

fn planned_follower_adoption_delay(
    poll: &foundationdb::recipes::leader_election::PollResult,
    local_state: &LocalState,
    adoption_delay: Option<AdoptionDelay>,
) -> Option<Duration> {
    let PollOutcome::Follower {
        owner,
        rank,
        lease_duration,
    } = poll.outcome()
    else {
        return None;
    };
    let is_new_or_reset = !matches!(
        local_state,
        LocalState::Observation(observation)
            if observation.owner() == owner
                && observation.rank() == *rank
                && observation.lease_duration() == *lease_duration
    );
    if is_new_or_reset {
        adoption_delay.map(|delay| delay.duration(*lease_duration))
    } else {
        None
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_resign(
    db: &SimDatabase,
    election: LeaderElection,
    log_subspace: Subspace,
    leadership: Leadership,
    now: Duration,
    client_id: i32,
    incarnation: u64,
    tracks_local_state: bool,
    op_num: u64,
) -> Result<bool, FdbBindingError> {
    let configured_lease_duration = election.lease_duration();
    db.run(|txn, _maybe_committed| {
        let election = election.clone();
        let log_subspace = log_subspace.clone();
        let leadership = leadership.clone();
        async move {
            txn.set_option(TransactionOption::AutomaticIdempotency)?;
            let prior =
                durable_from_public(election.state(&txn).await.map_err(FdbBindingError::from)?);
            let result = election
                .resign(&txn, &leadership)
                .await
                .map_err(FdbBindingError::from)?
                .is_resigned();
            let current =
                durable_from_public(election.state(&txn).await.map_err(FdbBindingError::from)?);
            let actor = leadership.participant().as_str().to_owned();
            let local_token = local_input(&LocalState::Leadership(leadership.clone()));
            write_log(
                &txn,
                &log_subspace,
                client_id,
                incarnation,
                op_num,
                OP_RESIGN,
                &actor,
                &prior,
                &current,
                &local_token,
                tracks_local_state,
                now,
                configured_lease_duration,
                None,
                TRANSITION_NONE,
                result,
                0,
                0,
                None,
                false,
                &[],
            );
            Ok::<_, FdbBindingError>(result)
        }
    })
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_observer(
    db: &SimDatabase,
    election: LeaderElection,
    log_subspace: Subspace,
    actor: String,
    configured_lease_duration: Duration,
    now: Duration,
    client_id: i32,
    incarnation: u64,
    op_num: u64,
) -> Result<(), FdbBindingError> {
    db.run(|txn, _maybe_committed| {
        let election = election.clone();
        let log_subspace = log_subspace.clone();
        let actor = actor.clone();
        async move {
            txn.set_option(TransactionOption::AutomaticIdempotency)?;
            let state =
                durable_from_public(election.state(&txn).await.map_err(FdbBindingError::from)?);
            write_log(
                &txn,
                &log_subspace,
                client_id,
                incarnation,
                op_num,
                OP_OBSERVE,
                &actor,
                &state,
                &state,
                &LocalInput::Unknown,
                false,
                now,
                configured_lease_duration,
                None,
                TRANSITION_NONE,
                true,
                0,
                0,
                None,
                false,
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
    now: Duration,
    client_id: i32,
    incarnation: u64,
    op_num: u64,
) -> Result<bool, FdbBindingError> {
    let configured_lease_duration = election.lease_duration();
    let payload = format!("stale:{actor}:{op_num}").into_bytes();
    db.run(|txn, _maybe_committed| {
        let election = election.clone();
        let register = register.clone();
        let log_subspace = log_subspace.clone();
        let actor = actor.clone();
        let payload = payload.clone();
        async move {
            txn.set_option(TransactionOption::AutomaticIdempotency)?;
            let state =
                durable_from_public(election.state(&txn).await.map_err(FdbBindingError::from)?);
            let write = register
                .write(&txn, stale_rank, &payload)
                .await
                .map_err(ranked_register_error)?;
            let readback = register
                .read(&txn, Rank::ZERO)
                .await
                .map_err(ranked_register_error)?;
            let readback_rank = readback.write_rank().as_u64();
            let readback_value = readback.into_value();
            write_log(
                &txn,
                &log_subspace,
                client_id,
                incarnation,
                op_num,
                OP_STALE_WRITE,
                &actor,
                &state,
                &state,
                &LocalInput::Unknown,
                false,
                now,
                configured_lease_duration,
                None,
                TRANSITION_NONE,
                write.is_committed(),
                stale_rank.as_u64(),
                readback_rank,
                readback_value.as_deref(),
                write.is_committed(),
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
    incarnation: u64,
    op_num: u64,
    kind: i64,
    actor: &str,
    prior: &DurableState,
    current: &DurableState,
    local: &LocalInput,
    tracks_local_state: bool,
    attempt_started_at: Duration,
    configured_lease_duration: Duration,
    planned_adoption_delay: Option<Duration>,
    transition: i64,
    result: bool,
    requested_write_rank: u64,
    observed_write_rank: u64,
    observed_value: Option<&[u8]>,
    protected_write_committed: bool,
    payload: &[u8],
) {
    let key = log_subspace.pack_with_versionstamp(&(
        Versionstamp::incomplete(0),
        client_id,
        incarnation,
        op_num,
    ));
    let value = pack(&(
        kind,
        actor,
        durable_wire(prior),
        durable_wire(current),
        local_wire(local),
        tracks_local_state,
        duration_wire(attempt_started_at),
        duration_wire(configured_lease_duration),
        optional_duration_wire(planned_adoption_delay),
        transition,
        result,
        (
            requested_write_rank,
            observed_write_rank,
            observed_value.is_some(),
            observed_value.unwrap_or(&[]),
            protected_write_committed,
            payload,
        ),
    ));
    txn.atomic_op(&key, &value, MutationType::SetVersionstampedKey);
}

fn decode_log_entry(
    log_subspace: &Subspace,
    key: &[u8],
    value: &[u8],
) -> Result<LogEntry, FdbBindingError> {
    let (versionstamp, client_id, incarnation, op_num): (Versionstamp, i32, u64, u64) =
        log_subspace
            .unpack(key)
            .map_err(FdbBindingError::PackError)?;
    let (
        kind,
        actor,
        prior,
        current,
        local,
        tracks_local_state,
        attempt_started_at,
        configured_lease_duration,
        planned_adoption_delay,
        transition,
        result,
        (
            requested_write_rank,
            observed_write_rank,
            observed_has_value,
            observed_value,
            protected_write_committed,
            payload,
        ),
    ): LogWire = unpack(value).map_err(FdbBindingError::PackError)?;
    Ok(LogEntry {
        versionstamp,
        client_id,
        incarnation,
        op_num,
        kind,
        actor,
        prior: durable_from_wire(prior)?,
        current: durable_from_wire(current)?,
        local_input: local_from_wire(local)?,
        tracks_local_state,
        attempt_started_at: duration_from_wire(attempt_started_at)?,
        configured_lease_duration: duration_from_wire(configured_lease_duration)?,
        planned_adoption_delay: optional_duration_from_wire(planned_adoption_delay)?,
        transition,
        result,
        requested_write_rank,
        observed_write_rank,
        observed_value: optional_value(observed_has_value, observed_value)?,
        protected_write_committed,
        payload,
    })
}

type DurationWire = (u64, u32);
type OptionalDurationWire = (bool, u64, u32);
type DurableWire = (u64, bool, String, bool, u64, u32);
type LocalWire = (i64, bool, String, u64, bool, u64, u32, u64, u32);
type ProtectedWire = (u64, u64, bool, Vec<u8>, bool, Vec<u8>);
type LogWire = (
    i64,
    String,
    DurableWire,
    DurableWire,
    LocalWire,
    bool,
    DurationWire,
    DurationWire,
    OptionalDurationWire,
    i64,
    bool,
    ProtectedWire,
);

fn replay(entries: &[LogEntry], snapshot: &Snapshot) -> Result<CoverageReport, String> {
    let mut seen = BTreeSet::new();
    let mut local_states = BTreeMap::new();
    let mut previous_versionstamp = None;
    let mut election = DurableState::default();
    let mut protected_rank = 0;
    let mut protected_value = None;
    let mut witnesses = ReplayWitnesses::default();

    for entry in entries {
        if !entry.versionstamp.is_complete()
            || previous_versionstamp
                .as_ref()
                .is_some_and(|previous| previous >= &entry.versionstamp)
        {
            return Err("operation log is not strictly commit ordered".to_owned());
        }
        previous_versionstamp = Some(entry.versionstamp.clone());
        if entry.actor.is_empty()
            || !seen.insert((entry.actor.clone(), entry.incarnation, entry.op_num))
        {
            return Err(format!(
                "invalid logical operation ({}, {})",
                entry.actor, entry.op_num
            ));
        }
        if entry.prior != election {
            return Err(format!(
                "operation ({}, {}) prior state {:?} differs from replay {:?}",
                entry.client_id, entry.op_num, entry.prior, election
            ));
        }
        if entry.configured_lease_duration.is_zero() {
            return Err(format!(
                "operation ({}, {}) configured zero lease",
                entry.client_id, entry.op_num
            ));
        }
        let delayed_observation = validate_local_input(entry, &local_states)?;

        match entry.kind {
            OP_POLL => replay_poll(
                entry,
                &mut election,
                &mut protected_rank,
                &mut protected_value,
                &mut witnesses,
                delayed_observation,
            )?,
            OP_RESIGN => replay_resign(entry, &mut election, &mut witnesses)?,
            OP_OBSERVE => {
                if entry.current != election
                    || entry.local_input != LocalInput::Unknown
                    || entry.transition != TRANSITION_NONE
                    || !entry.result
                    || has_protected_effect(entry)
                {
                    return Err(format!(
                        "observer ({}, {}) changed state",
                        entry.client_id, entry.op_num
                    ));
                }
            }
            OP_STALE_WRITE => {
                if entry.current != election
                    || entry.local_input != LocalInput::Unknown
                    || entry.transition != TRANSITION_NONE
                    || entry.result
                    || entry.requested_write_rank >= election.rank
                    || entry.observed_write_rank != protected_rank
                    || entry.observed_value != protected_value
                    || entry.protected_write_committed
                    || entry.payload != stale_payload(&entry.actor, entry.op_num)
                {
                    return Err(format!(
                        "stale write ({}, {}) was not rejected",
                        entry.client_id, entry.op_num
                    ));
                }
                witnesses.saw_stale_write = true;
                if entry.requested_write_rank == 0 {
                    witnesses.saw_zero_rank_stale_write = true;
                } else {
                    witnesses.saw_post_advance_stale_write = true;
                }
            }
            other => return Err(format!("unknown operation kind {other}")),
        }
        advance_local_state(entry, &election, &mut local_states)?;
    }

    if snapshot.election != election
        || snapshot.protected_rank != protected_rank
        || snapshot.protected_value != protected_value
    {
        return Err(
            "final public election or ranked-register state differs from replay".to_owned(),
        );
    }
    if protected_value.is_none() {
        return Err("no fenced leader write committed".to_owned());
    }
    let mut missing = Vec::new();
    if !witnesses.saw_expiry {
        missing.push("expiry");
    }
    if !witnesses.saw_foreign_takeover {
        missing.push("foreign_takeover");
    }
    if !witnesses.saw_before_expiry {
        missing.push("before_expiry");
    }
    if !witnesses.saw_exact_expiry {
        missing.push("exact_expiry");
    }
    if !witnesses.saw_after_expiry {
        missing.push("after_expiry");
    }
    if !witnesses.saw_stale_renew {
        missing.push("stale_renew");
    }
    if !witnesses.saw_stale_renew_after_advance {
        missing.push("stale_renew_after_advance");
    }
    if !witnesses.saw_stale_resign {
        missing.push("stale_resign");
    }
    if !witnesses.saw_delayed_stale_resign {
        missing.push("delayed_stale_resign");
    }
    if !witnesses.saw_stale_resign_after_takeover {
        missing.push("stale_resign_after_takeover");
    }
    if !witnesses.saw_exact_resign {
        missing.push("exact_resign");
    }
    if !witnesses.saw_stale_write {
        missing.push("stale_write");
    }
    if !witnesses.saw_zero_rank_stale_write {
        missing.push("zero_rank_stale_write");
    }
    if !witnesses.saw_post_advance_stale_write {
        missing.push("post_advance_stale_write");
    }
    if !witnesses.saw_follower_duration_reset {
        missing.push("follower_duration_reset");
    }
    if !witnesses.saw_delayed_observation_adoption {
        missing.push("delayed_observation_adoption");
    }
    Ok(CoverageReport {
        observed: 16 - missing.len(),
        missing,
    })
}

fn replay_poll(
    entry: &LogEntry,
    election: &mut DurableState,
    protected_rank: &mut u64,
    protected_value: &mut Option<Vec<u8>>,
    witnesses: &mut ReplayWitnesses,
    delayed_observation: bool,
) -> Result<(), String> {
    let leader = validate_reported_poll(entry, election)?;

    if leader {
        let expected_rank = election.rank.checked_add(1).ok_or("revision overflow")?;
        let expected = DurableState {
            rank: expected_rank,
            owner: Some(entry.actor.clone()),
            lease_duration: Some(entry.configured_lease_duration),
        };
        if entry.current != expected
            || entry.requested_write_rank != expected_rank
            || entry.observed_write_rank != *protected_rank
            || entry.observed_value != *protected_value
            || !entry.protected_write_committed
            || entry.payload != protected_payload(&entry.actor, entry.op_num)
        {
            return Err(format!(
                "leader poll ({}, {}) has invalid durable or fenced-write evidence",
                entry.client_id, entry.op_num
            ));
        }
        if matches!(
            entry.transition,
            TRANSITION_TOOK_OVER | TRANSITION_REACQUIRED
        ) {
            witnesses.saw_expiry = true;
        }
        if entry.transition == TRANSITION_TOOK_OVER {
            witnesses.saw_foreign_takeover = true;
            witnesses.last_takeover_rank = Some(expected.rank);
        }
        if entry.transition == TRANSITION_RENEWED
            && election.lease_duration != expected.lease_duration
        {
            witnesses.renewal_with_new_duration_rank = Some(expected.rank);
        }
        if let LocalInput::Observation {
            owner,
            rank,
            lease_duration,
            observed_at,
        } = &entry.local_input
        {
            if election.owner.as_deref() == Some(owner)
                && election.rank == *rank
                && election.lease_duration == Some(*lease_duration)
            {
                let elapsed = entry.attempt_started_at.saturating_sub(*observed_at);
                match elapsed.cmp(lease_duration) {
                    std::cmp::Ordering::Equal => witnesses.saw_exact_expiry = true,
                    std::cmp::Ordering::Greater => witnesses.saw_after_expiry = true,
                    std::cmp::Ordering::Less => {}
                }
            }
        }
        *election = expected;
        *protected_rank = expected_rank;
        *protected_value = Some(entry.payload.clone());
    } else {
        if entry.current != *election || has_protected_effect(entry) {
            return Err(format!(
                "follower poll ({}, {}) changed state",
                entry.client_id, entry.op_num
            ));
        }
        if let LocalInput::Observation {
            owner,
            rank,
            lease_duration,
            observed_at,
        } = &entry.local_input
        {
            if election.owner.as_deref() == Some(owner)
                && election.rank == *rank
                && election.lease_duration == Some(*lease_duration)
                && entry.attempt_started_at.saturating_sub(*observed_at) < *lease_duration
            {
                witnesses.saw_before_expiry = true;
                if delayed_observation {
                    witnesses.saw_delayed_observation_adoption = true;
                }
            }
        }
        if let LocalInput::Leadership {
            participant,
            rank,
            lease_duration,
            renewed_at,
        } = &entry.local_input
        {
            if participant == &entry.actor {
                witnesses.saw_stale_renew = true;
                if election.owner.as_deref() == Some(entry.actor.as_str())
                    && election.rank == *rank
                    && election.lease_duration == Some(*lease_duration)
                    && entry.attempt_started_at.saturating_sub(*renewed_at) >= *lease_duration
                {
                    witnesses.saw_after_expiry = true;
                }
                if *rank < election.rank {
                    witnesses.saw_stale_renew_after_advance = true;
                }
            }
        }
    }
    if !leader {
        if let Some(changed_rank) = witnesses.renewal_with_new_duration_rank {
            if election.rank == changed_rank
                && matches!(
                    &entry.local_input,
                    LocalInput::Unknown | LocalInput::Leadership { .. }
                )
            {
                witnesses.saw_follower_duration_reset = true;
            }
        }
    }
    Ok(())
}

fn replay_resign(
    entry: &LogEntry,
    election: &mut DurableState,
    witnesses: &mut ReplayWitnesses,
) -> Result<(), String> {
    let LocalInput::Leadership {
        participant,
        rank,
        lease_duration,
        ..
    } = &entry.local_input
    else {
        return Err(format!(
            "resign ({}, {}) lacks leadership token",
            entry.client_id, entry.op_num
        ));
    };
    let matches = election.owner.as_deref() == Some(participant)
        && election.rank == *rank
        && election.lease_duration == Some(*lease_duration);
    let expected = if matches {
        DurableState {
            rank: election.rank,
            owner: None,
            lease_duration: election.lease_duration,
        }
    } else {
        election.clone()
    };
    if entry.transition != TRANSITION_NONE
        || entry.result != matches
        || entry.current != expected
        || has_protected_effect(entry)
    {
        return Err(format!(
            "resign ({}, {}) has invalid result",
            entry.client_id, entry.op_num
        ));
    }
    if !matches {
        witnesses.saw_stale_resign = true;
        if let LocalInput::Leadership {
            renewed_at,
            lease_duration,
            ..
        } = &entry.local_input
        {
            if entry.attempt_started_at.saturating_sub(*renewed_at) > *lease_duration {
                witnesses.saw_delayed_stale_resign = true;
            }
        }
        if witnesses
            .last_takeover_rank
            .is_some_and(|takeover_rank| *rank < takeover_rank)
        {
            witnesses.saw_stale_resign_after_takeover = true;
        }
    } else {
        witnesses.saw_exact_resign = true;
    }
    *election = expected;
    Ok(())
}

fn validate_local_input(
    entry: &LogEntry,
    local_states: &BTreeMap<(String, u64), LocalExpectation>,
) -> Result<bool, String> {
    if !entry.tracks_local_state {
        return Ok(false);
    }
    let key = (entry.actor.clone(), entry.incarnation);
    match local_states.get(&key) {
        None if entry.local_input == LocalInput::Unknown => Ok(false),
        None => Err(format!(
            "incarnation ({}, {}) adopted local state without a prior poll",
            entry.actor, entry.incarnation
        )),
        Some(LocalExpectation::Exact(expected)) if expected == &entry.local_input => Ok(false),
        Some(LocalExpectation::AdoptedObservation {
            owner,
            rank,
            lease_duration,
            not_before,
            planned_adoption_delay,
        }) => match &entry.local_input {
            LocalInput::Observation {
                owner: actual_owner,
                rank: actual_rank,
                lease_duration: actual_duration,
                observed_at,
            } if actual_owner == owner
                && actual_rank == rank
                && actual_duration == lease_duration
                && *observed_at > *not_before
                && *observed_at
                    >= not_before.saturating_add(planned_adoption_delay.unwrap_or_default())
                && *observed_at <= entry.attempt_started_at =>
            {
                Ok(planned_adoption_delay.is_some_and(|delay| delay > *lease_duration))
            }
            _ => Err(format!(
                "incarnation ({}, {}) did not adopt its follower observation",
                entry.actor, entry.incarnation
            )),
        },
        Some(_) => Err(format!(
            "incarnation ({}, {}) local state regressed or changed unexpectedly",
            entry.actor, entry.incarnation
        )),
    }
}

fn advance_local_state(
    entry: &LogEntry,
    election: &DurableState,
    local_states: &mut BTreeMap<(String, u64), LocalExpectation>,
) -> Result<(), String> {
    if !entry.tracks_local_state {
        return Ok(());
    }
    let key = (entry.actor.clone(), entry.incarnation);
    let next = match entry.kind {
        OP_POLL if entry.result => {
            if entry.planned_adoption_delay.is_some() {
                return Err("leader poll planned a follower adoption delay".to_owned());
            }
            LocalExpectation::Exact(LocalInput::Leadership {
                participant: entry.actor.clone(),
                rank: election.rank,
                lease_duration: election
                    .lease_duration
                    .ok_or_else(|| "leader poll lost lease duration".to_owned())?,
                renewed_at: entry.attempt_started_at,
            })
        }
        OP_POLL => match &entry.local_input {
            LocalInput::Observation {
                owner,
                rank,
                lease_duration,
                ..
            } if election.owner.as_deref() == Some(owner)
                && election.rank == *rank
                && election.lease_duration == Some(*lease_duration) =>
            {
                if entry.planned_adoption_delay.is_some() {
                    return Err("preserved follower observation planned a delay".to_owned());
                }
                LocalExpectation::Exact(entry.local_input.clone())
            }
            _ => follower_observation_expectation(
                election,
                entry.attempt_started_at,
                entry.planned_adoption_delay,
            )?,
        },
        OP_RESIGN if entry.result => LocalExpectation::Exact(LocalInput::Unknown),
        OP_RESIGN => LocalExpectation::Exact(entry.local_input.clone()),
        _ => return Ok(()),
    };
    local_states.insert(key, next);
    Ok(())
}

fn follower_observation_expectation(
    election: &DurableState,
    not_before: Duration,
    planned_adoption_delay: Option<Duration>,
) -> Result<LocalExpectation, String> {
    Ok(LocalExpectation::AdoptedObservation {
        owner: election
            .owner
            .clone()
            .ok_or_else(|| "follower poll lost owner".to_owned())?,
        rank: election.rank,
        lease_duration: election
            .lease_duration
            .ok_or_else(|| "follower poll lost lease duration".to_owned())?,
        not_before,
        planned_adoption_delay,
    })
}

fn validate_reported_poll(entry: &LogEntry, election: &DurableState) -> Result<bool, String> {
    let valid = match entry.transition {
        TRANSITION_ACQUIRED => entry.result && election.owner.is_none(),
        TRANSITION_RENEWED => {
            if !matches!(&entry.local_input, LocalInput::Leadership { .. }) {
                return Err(poll_transition_error(
                    entry,
                    "renewal lacks a leadership token",
                ));
            }
            entry.result && renewal_enabled(entry, election)
        }
        TRANSITION_TOOK_OVER | TRANSITION_REACQUIRED => {
            let LocalInput::Observation { owner, .. } = &entry.local_input else {
                return Err(poll_transition_error(
                    entry,
                    "takeover lacks an observation",
                ));
            };
            entry.result
                && takeover_enabled(entry, election)
                && match entry.transition {
                    TRANSITION_TOOK_OVER => owner != &entry.actor,
                    TRANSITION_REACQUIRED => owner == &entry.actor,
                    _ => unreachable!(),
                }
        }
        TRANSITION_FOLLOWED => {
            !entry.result
                && election.owner.is_some()
                && !renewal_enabled(entry, election)
                && !takeover_enabled(entry, election)
        }
        other => return Err(format!("unknown poll transition {other}")),
    };
    if valid {
        Ok(entry.result)
    } else {
        Err(poll_transition_error(
            entry,
            "reported transition violates its safety preconditions",
        ))
    }
}

fn renewal_enabled(entry: &LogEntry, election: &DurableState) -> bool {
    let LocalInput::Leadership {
        participant,
        rank,
        lease_duration,
        renewed_at,
    } = &entry.local_input
    else {
        return false;
    };
    participant == &entry.actor
        && election.owner.as_deref() == Some(entry.actor.as_str())
        && election.rank == *rank
        && election.lease_duration == Some(*lease_duration)
        && entry.attempt_started_at.saturating_sub(*renewed_at) < *lease_duration
}

fn takeover_enabled(entry: &LogEntry, election: &DurableState) -> bool {
    let LocalInput::Observation {
        owner,
        rank,
        lease_duration,
        observed_at,
    } = &entry.local_input
    else {
        return false;
    };
    election.owner.as_deref() == Some(owner)
        && election.rank == *rank
        && election.lease_duration == Some(*lease_duration)
        && entry.attempt_started_at.saturating_sub(*observed_at) >= *lease_duration
}

fn poll_transition_error(entry: &LogEntry, message: &str) -> String {
    format!(
        "poll ({}, {}) transition {} invalid: {message}",
        entry.client_id, entry.op_num, entry.transition
    )
}

fn has_protected_effect(entry: &LogEntry) -> bool {
    entry.requested_write_rank != 0
        || entry.observed_write_rank != 0
        || entry.observed_value.is_some()
        || entry.protected_write_committed
        || !entry.payload.is_empty()
}

fn durable_from_public(state: ElectionState) -> DurableState {
    DurableState {
        rank: state.rank().as_u64(),
        owner: state.owner().map(|owner| owner.as_str().to_owned()),
        lease_duration: state.lease_duration(),
    }
}

fn local_input(state: &LocalState) -> LocalInput {
    match state {
        LocalState::Unknown => LocalInput::Unknown,
        LocalState::Observation(observation) => LocalInput::Observation {
            owner: observation.owner().as_str().to_owned(),
            rank: observation.rank().as_u64(),
            lease_duration: observation.lease_duration(),
            observed_at: observation.first_observed_at(),
        },
        LocalState::Leadership(leadership) => LocalInput::Leadership {
            participant: leadership.participant().as_str().to_owned(),
            rank: leadership.rank().as_u64(),
            lease_duration: leadership.lease_duration(),
            renewed_at: leadership.last_renewed_at(),
        },
    }
}

fn transition_code(transition: PollTransition) -> i64 {
    match transition {
        PollTransition::Acquired => TRANSITION_ACQUIRED,
        PollTransition::Renewed => TRANSITION_RENEWED,
        PollTransition::TookOver => TRANSITION_TOOK_OVER,
        PollTransition::Reacquired => TRANSITION_REACQUIRED,
        PollTransition::Followed => TRANSITION_FOLLOWED,
    }
}

fn durable_wire(state: &DurableState) -> DurableWire {
    let (has_lease_duration, lease_secs, lease_nanos) = match state.lease_duration {
        Some(duration) => (true, duration.as_secs(), duration.subsec_nanos()),
        None => (false, 0, 0),
    };
    (
        state.rank,
        state.owner.is_some(),
        state.owner.as_deref().unwrap_or("").to_owned(),
        has_lease_duration,
        lease_secs,
        lease_nanos,
    )
}

fn durable_from_wire(wire: DurableWire) -> Result<DurableState, FdbBindingError> {
    let (rank, has_owner, owner, has_lease, lease_secs, lease_nanos) = wire;
    let owner = optional_owner(has_owner, owner)?;
    let lease_duration = optional_duration(has_lease, lease_secs, lease_nanos)?;
    Ok(DurableState {
        rank,
        owner,
        lease_duration,
    })
}

fn local_wire(local: &LocalInput) -> LocalWire {
    match local {
        LocalInput::Unknown => (LOCAL_UNKNOWN, false, String::new(), 0, false, 0, 0, 0, 0),
        LocalInput::Observation {
            owner,
            rank,
            lease_duration,
            observed_at,
        } => (
            LOCAL_OBSERVATION,
            true,
            owner.clone(),
            *rank,
            true,
            lease_duration.as_secs(),
            lease_duration.subsec_nanos(),
            observed_at.as_secs(),
            observed_at.subsec_nanos(),
        ),
        LocalInput::Leadership {
            participant,
            rank,
            lease_duration,
            renewed_at,
        } => (
            LOCAL_LEADERSHIP,
            true,
            participant.clone(),
            *rank,
            true,
            lease_duration.as_secs(),
            lease_duration.subsec_nanos(),
            renewed_at.as_secs(),
            renewed_at.subsec_nanos(),
        ),
    }
}

fn local_from_wire(wire: LocalWire) -> Result<LocalInput, FdbBindingError> {
    let (kind, has_actor, actor, rank, has_lease, lease_secs, lease_nanos, at_secs, at_nanos) =
        wire;
    let actor = optional_owner(has_actor, actor)?;
    let lease_duration = optional_duration(has_lease, lease_secs, lease_nanos)?;
    let at = duration_from_wire((at_secs, at_nanos))?;
    match kind {
        LOCAL_UNKNOWN
            if actor.is_none() && rank == 0 && lease_duration.is_none() && at.is_zero() =>
        {
            Ok(LocalInput::Unknown)
        }
        LOCAL_OBSERVATION => Ok(LocalInput::Observation {
            owner: actor.ok_or_else(|| log_error("observation has no owner"))?,
            rank,
            lease_duration: lease_duration.ok_or_else(|| log_error("observation has no lease"))?,
            observed_at: at,
        }),
        LOCAL_LEADERSHIP => Ok(LocalInput::Leadership {
            participant: actor.ok_or_else(|| log_error("leadership has no participant"))?,
            rank,
            lease_duration: lease_duration.ok_or_else(|| log_error("leadership has no lease"))?,
            renewed_at: at,
        }),
        _ => Err(log_error("invalid caller-local input in operation log")),
    }
}

fn duration_wire(duration: Duration) -> DurationWire {
    (duration.as_secs(), duration.subsec_nanos())
}

fn duration_from_wire(wire: DurationWire) -> Result<Duration, FdbBindingError> {
    if wire.1 >= 1_000_000_000 {
        return Err(log_error("duration nanoseconds out of range"));
    }
    Ok(Duration::new(wire.0, wire.1))
}

fn optional_duration_wire(duration: Option<Duration>) -> OptionalDurationWire {
    match duration {
        Some(duration) => (true, duration.as_secs(), duration.subsec_nanos()),
        None => (false, 0, 0),
    }
}

fn optional_duration_from_wire(
    wire: OptionalDurationWire,
) -> Result<Option<Duration>, FdbBindingError> {
    optional_duration(wire.0, wire.1, wire.2)
}

fn optional_duration(
    present: bool,
    secs: u64,
    nanos: u32,
) -> Result<Option<Duration>, FdbBindingError> {
    if present {
        Ok(Some(duration_from_wire((secs, nanos))?))
    } else if secs == 0 && nanos == 0 {
        Ok(None)
    } else {
        Err(log_error("absent duration has non-zero fields"))
    }
}

fn optional_owner(present: bool, owner: String) -> Result<Option<String>, FdbBindingError> {
    match (present, owner.is_empty()) {
        (true, false) => Ok(Some(owner)),
        (false, true) => Ok(None),
        _ => Err(log_error("invalid optional owner in operation log")),
    }
}

fn optional_value(present: bool, value: Vec<u8>) -> Result<Option<Vec<u8>>, FdbBindingError> {
    match (present, value.is_empty()) {
        (true, _) => Ok(Some(value)),
        (false, true) => Ok(None),
        (false, false) => Err(log_error("invalid optional value in operation log")),
    }
}

fn protected_payload(actor: &str, op_num: u64) -> Vec<u8> {
    format!("leader:{actor}:{op_num}").into_bytes()
}

fn stale_payload(actor: &str, op_num: u64) -> Vec<u8> {
    format!("stale:{actor}:{op_num}").into_bytes()
}

async fn delay_until(context: &WorkloadContext, target: Duration) {
    let _ = context
        .delay(target.saturating_sub(simulated_now(context)))
        .await;
}

fn simulated_now(context: &WorkloadContext) -> Duration {
    Duration::from_secs_f64(context.now().max(0.0))
}

fn ranked_register_error(error: RankedRegisterError) -> FdbBindingError {
    FdbBindingError::new_custom_error(Box::new(error))
}

fn log_error(message: &str) -> FdbBindingError {
    FdbBindingError::new_custom_error(Box::new(LogError(message.to_owned())))
}

#[derive(Debug)]
struct LogError(String);

impl std::fmt::Display for LogError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for LogError {}
