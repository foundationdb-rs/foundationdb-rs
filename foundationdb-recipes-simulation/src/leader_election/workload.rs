//! The simulation workload itself.
//!
//! Three phases. `setup` publishes what the run was configured with and lines
//! the clients up so none of them starts a lease while the others are still
//! being created. `start` hands each client to its [role](super::roles) until
//! the simulated deadline. `check` reads the whole log back, replays it, and
//! judges the run against [`invariants`](super::invariants).
//!
//! # Where a failure comes from
//!
//! `Severity::Error` is the only thing that fails a FoundationDB simulation
//! run, and this workload emits it in exactly two situations: an invariant was
//! violated, or the check phase could not obtain the evidence to judge (an
//! unreadable log, a configuration it does not understand). Everything else,
//! including a client whose role died on an infrastructure error, is a warning:
//! what that client failed to do shows up as missing progress, and
//! `ProgressMade` is what decides whether that mattered.
//!
//! The check runs on *every* client, not just client 0. Attrition kills
//! clients, and a run whose only judge was killed used to pass by default.
//!
//! # Options
//!
//! Every knob is read exactly once, in [`new`](LeaderElectionWorkload::new).
//! `get_option` consumes, and fdbserver fails a run that leaves options
//! unconsumed, so a misspelled knob is a failed run rather than a silently
//! ignored setting. That is also why the anchor configurations all carry the
//! same knobs even where a value does nothing: an unread knob would fail the
//! run it is irrelevant to.
//!
//! There are two families of knobs, and a run belongs to exactly one of them.
//! `swarmEnabled` and `testDurationSecs` are read first and always. When
//! `swarmEnabled` is set nothing else is read at all: everything the run does
//! is drawn from the seed the simulator shares with every client, so a swarm
//! file carries those two knobs and no others. Otherwise the ten remaining
//! knobs are read: an anchor file spells out all eleven, and the run is exactly
//! what its file says it is.
//!
//! That asymmetry is not a convenience. A swarm file that also carried, say, a
//! lease would be a file whose lease is silently ignored, which is the failure
//! mode the consume-once discipline exists to prevent; and the plan has to come
//! from the seed alone for a failing seed to reproduce.

use std::sync::Arc;
use std::time::Duration;

use foundationdb::options::{StreamingMode, TransactionOption};
use foundationdb::recipes::leader_election::{
    DEFAULT_HISTORY_RETENTION, HistoryEvent, HistoryEventKind, LeaderElection, LeaderRecord,
    LeaseDuration,
};
use foundationdb::recipes::ranked_register::RankedRegister;
use foundationdb::tuple::{Subspace, pack};
use foundationdb::{FdbBindingError, RangeOption, RetryableTransaction};
use foundationdb_simulation::{
    Metric, Metrics, RustWorkload, Severity, SimDatabase, SingleRustWorkload, WorkloadContext,
    details,
};
use futures::TryStreamExt;

use super::clock::{SkewMode, SkewedClock};
use super::elector_invariants::{
    ELECTOR_INVARIANTS, ElectorEvidence, ElectorSnapshot, ElectorThresholds, StampedTransition,
    check_elector, first_judgeable_ballot, merge, writes_outside_the_window,
};
use super::elector_role;
use super::invariants::{
    CheckInputs, HistoryEntry, HistoryKind, InvariantReport, ProgressThresholds, Tolerances,
    check_all, is_resolution,
};
use super::log_schema::{LogEntry, OpKind, elector_log_subspace, log_subspace};
use super::logged_op::{Journal, op_ceiling};
use super::replay::{ExpectedRecord, TransitionKind, replay};
use super::roles::{Driver, DriverConfig, ForcedRecoveryConfig, Role, elector_clients};
use super::swarm::{FaultTiming, SwarmPlan};

/// How many transition records the check phase asks the recipe for
///
/// Twice the recipe's retention bound, which is what actually limits the trail:
/// both elections run on [`DEFAULT_HISTORY_RETENTION`], so a run of any length
/// hands back a suffix of about that many entries and never the whole trail.
/// The doubling is for the trimming being lazy, so the stored count overshoots
/// the bound at the margin; asking for thousands, as this used to, only
/// suggested the trail might arrive whole. It never does, and
/// [`history_faithful`](super::invariants::history_faithful) is written to
/// compare a suffix.
const HISTORY_LIMIT: usize = DEFAULT_HISTORY_RETENTION * 2;
/// How many violations of one invariant are spelled out before the rest are
/// summarised
const MAX_VIOLATIONS_TRACED: usize = 5;
/// How many entries either side of the first violation are dumped
const DUMP_RADIUS: usize = 10;

/// Drives leader election against the simulated cluster
pub struct LeaderElectionWorkload {
    context: WorkloadContext,
    client_id: i32,
    client_count: i32,
    election: LeaderElection,
    /// The election the elector role campaigns in, for the check phase
    elector_election: LeaderElection,
    config: DriverConfig,
    thresholds: ProgressThresholds,
    role: Role,
    /// Which clients ran the recipe's own elector
    ///
    /// A pure function of the plan and the field size, computed identically on
    /// every client. Empty means the elector half of the check phase has
    /// nothing to judge, and says so rather than passing silently.
    electors: Vec<i32>,
    driver: Driver,
    /// The plan the seed drew, when this is a swarm run
    ///
    /// Everything it decided is already folded into `config`, `role` and
    /// `thresholds`; it is kept whole so that `setup` can publish it, which is
    /// what makes a failing seed reproducible.
    plan: Option<SwarmPlan>,
    /// A configuration this build cannot honour, reported in `setup` where
    /// there is a trace sink to report it to
    config_error: Option<String>,
}

impl SingleRustWorkload for LeaderElectionWorkload {
    fn new(_name: String, context: WorkloadContext) -> Self {
        let client_id = context.client_id();
        let client_count = context.client_count();
        let env = context.environment();

        // ------------------------------------------------------------------
        // The two knobs every configuration carries, read first because the
        // first of them decides which family the rest of the run belongs to.
        // ------------------------------------------------------------------
        let swarm_enabled: bool = context.get_option("swarmEnabled").unwrap_or(false);
        let test_duration_secs: f64 = context.get_option("testDurationSecs").unwrap_or(60.0);

        let mut config_error = None;
        let test_duration = secs_or_default(
            "testDurationSecs",
            test_duration_secs,
            DEFAULT_TEST_DURATION_SECS,
            &mut config_error,
        );
        // The shared number, not a draw: it is the one value every client of a
        // run agrees on, so all of them plan identically without coordinating.
        let plan = swarm_enabled
            .then(|| SwarmPlan::draw(context.shared_random_number() as u64, test_duration));

        let (config, role, thresholds) = match &plan {
            Some(plan) => {
                let sleeper = plan.features.sleeper;
                let watcher = plan.features.watcher;
                let real_elector = plan.features.real_elector;
                let config = DriverConfig {
                    lease: lease_or_default("the drawn lease", plan.lease_secs, &mut config_error),
                    step: plan.step(),
                    test_duration,
                    resign: plan.resign.clone(),
                    crash: plan.crash.clone(),
                    pause_factor: plan.pause_factor,
                    skew_mode: plan.skew_mode,
                    sleeper_head_start: head_start(
                        Role::assign(1, client_count, sleeper, watcher, real_elector),
                        plan.step_secs,
                        plan.lease_secs,
                    ),
                    forced_recovery: ForcedRecoveryConfig {
                        enabled: plan.features.forced_recovery,
                        // Rare after the first, which every contender takes
                        // unconditionally: a client that spends the run
                        // recovering never gets far enough to be stolen from.
                        probability: 0.10,
                        // Past a lease, so the late injections reach the
                        // terminal half of the recovery contract.
                        max_delay_leases: 1.5,
                    },
                };
                let role = Role::assign(client_id, client_count, sleeper, watcher, real_elector);
                // Derived from the plan rather than configured: a run has to
                // prove exactly as much as the churn it drew makes possible.
                (config, role, plan.thresholds(client_count))
            }
            None => {
                // ----------------------------------------------------------
                // Every knob of every anchor configuration, read exactly once.
                // ----------------------------------------------------------
                let lease_secs: f64 = context.get_option("leaseDurationSecs").unwrap_or(10.0);
                let step_secs: f64 = context.get_option("stepIntervalSecs").unwrap_or(1.0);
                let resign_probability: f64 =
                    context.get_option("resignProbability").unwrap_or(0.1);
                let crash_probability: f64 = context.get_option("crashProbability").unwrap_or(0.0);
                let clock_skew_mode: String = context
                    .get_option("clockSkewMode")
                    .unwrap_or_else(|| "none".to_string());
                let pause_factor: f64 = context.get_option("pauseFactor").unwrap_or(2.0);
                let sleeper_enabled: bool = context.get_option("sleeperEnabled").unwrap_or(false);
                let min_acquisitions: usize =
                    context.get_option("minLeadershipClaims").unwrap_or(2);
                let min_renewals: usize = context.get_option("minRenewals").unwrap_or(2);
                let min_observed_identities: usize =
                    context.get_option("minObservedIdentities").unwrap_or(2);

                let skew_mode = SkewMode::parse(&clock_skew_mode).unwrap_or_else(|| {
                    config_error = Some(format!(
                        "clockSkewMode {clock_skew_mode:?} is not one of none, random, extreme"
                    ));
                    SkewMode::None
                });
                let lease = lease_or_default("leaseDurationSecs", lease_secs, &mut config_error);

                let config = DriverConfig {
                    lease,
                    // Validated before anything is derived from it. A zero step
                    // is an unpaced role loop, and the ceiling and the liveness
                    // guard are both computed from it.
                    step: secs_or_default(
                        "stepIntervalSecs",
                        step_secs,
                        DEFAULT_STEP_SECS,
                        &mut config_error,
                    ),
                    test_duration,
                    // A flat per-step probability, which is what these files
                    // were written against and what `Constant` reproduces.
                    resign: FaultTiming::Constant(resign_probability),
                    crash: FaultTiming::Constant(crash_probability),
                    pause_factor,
                    skew_mode,
                    // An anchor run always assigns the Watcher: unlike a swarm
                    // run it has no feature to draw, and the file that wants a
                    // field of pure contenders simply says so with its client
                    // count.
                    sleeper_head_start: head_start(
                        // No elector: the anchor files are the configurations a
                        // human reasoned about, and running two elections at
                        // once is not one of them.
                        Role::assign(1, client_count, sleeper_enabled, true, false),
                        step_secs,
                        lease_secs,
                    ),
                    // Not a knob. The anchor files are the configurations a
                    // human reasoned about, and an injected unknown commit is
                    // exactly the thing nobody can reason about the timing of;
                    // it belongs to the seeds, not to the files.
                    forced_recovery: ForcedRecoveryConfig::disabled(),
                };
                let role = Role::assign(client_id, client_count, sleeper_enabled, true, false);
                let thresholds = ProgressThresholds {
                    min_acquisitions,
                    min_renewals,
                    min_observed_identities,
                    // Nothing injects here, so demanding a recovery would fail
                    // every anchor run for a path they cannot reach.
                    min_recoveries: 0,
                    // Not a knob: it has to be the interval the driver actually
                    // renews on (`roles.rs`), or the check would excuse runs
                    // that did have the chance to renew.
                    renew_interval: lease.as_duration() / 3,
                };
                (config, role, thresholds)
            }
        };

        let clock = SkewedClock::new(
            Arc::clone(env.clock()),
            config.skew_mode,
            config.lease.as_duration(),
            config.test_duration,
            || env.rng().next_u32(),
        );
        let election = LeaderElection::new(Subspace::all().subspace(&("leader_election",)));
        let journal = Journal::new(
            env,
            Arc::new(clock),
            election.clone(),
            RankedRegister::new(Subspace::all().subspace(&("le_register",))),
            log_subspace(),
            client_id,
            op_ceiling(config.test_duration, config.step),
        );
        let electors = match &plan {
            Some(plan) => elector_clients(
                client_count,
                plan.features.sleeper,
                plan.features.watcher,
                plan.features.real_elector,
            ),
            None => Vec::new(),
        };
        Self {
            driver: Driver::new(context.clone(), journal, config.clone(), role),
            context,
            client_id,
            client_count,
            election,
            elector_election: elector_role::election(),
            config,
            thresholds,
            role,
            electors,
            plan,
            config_error,
        }
    }
}

/// The longest a configured duration may be
///
/// A day. Nothing in this suite runs for one, and the point of an upper bound is
/// that `Duration::from_secs_f64` panics rather than saturates on a number too
/// large to represent.
const MAX_CONFIGURABLE_SECS: f64 = 86_400.0;

/// The run length a configuration that did not say gets
const DEFAULT_TEST_DURATION_SECS: f64 = 60.0;

/// The step a configuration that did not say gets
const DEFAULT_STEP_SECS: f64 = 1.0;

/// A duration from a configured number of seconds, or the default and a
/// complaint
///
/// Every `f64` knob goes through here, and it is deliberately strict.
/// `Duration::from_secs_f64` **panics** on an infinite or oversized value, and a
/// workload that panics in `new` takes the simulation down with a message about
/// floats instead of one about the file that asked for it. Zero is refused too:
/// a zero step is a role loop that never waits, which is the hot loop this
/// suite is hardened against, and it is also the denominator of the journal's
/// operation ceiling.
///
/// The substituted default keeps the rest of `new` working on sane numbers. It
/// does not rescue the run: `config_error` becomes a `Severity::Error` in
/// `setup`, which fails it.
fn secs_or_default(source: &str, secs: f64, default: f64, error: &mut Option<String>) -> Duration {
    if secs.is_finite() && secs > 0.0 && secs <= MAX_CONFIGURABLE_SECS {
        return Duration::from_secs_f64(secs);
    }
    *error = Some(format!(
        "{source} {secs} is not a usable number of seconds: \
         it must be finite, above zero and at most {MAX_CONFIGURABLE_SECS}"
    ));
    Duration::from_secs_f64(default)
}

/// The lease `secs` configures, or ten seconds and a complaint
///
/// `new` has no trace sink, so a configuration this build cannot honour has to
/// survive as a message until `setup` can report it. `source` names where the
/// value came from, which is the difference between a typo in a file and a plan
/// this build drew and then could not use.
fn lease_or_default(source: &str, secs: f64, error: &mut Option<String>) -> LeaseDuration {
    // Through the same validation first: `LeaseDuration::new` takes a
    // `Duration`, so an infinite `secs` would have panicked on the way in.
    let duration = secs_or_default(source, secs, 10.0, error);
    LeaseDuration::new(duration).unwrap_or_else(|problem| {
        *error = Some(format!("{source} {secs} is unusable: {problem}"));
        LeaseDuration::new(Duration::from_secs(10)).expect("ten seconds is a valid lease")
    })
}

/// How long the other roles hold back so the Sleeper can take the first term
///
/// Only when a Sleeper was actually assigned: the head start is dead time in
/// every other configuration.
fn head_start(sleeper: Role, step_secs: f64, lease_secs: f64) -> Duration {
    match sleeper {
        // Long enough to cover one slow first commit: under contention an
        // opening claim can take a good fraction of a lease to land, and a head
        // start shorter than that decides nothing.
        Role::Sleeper => Duration::from_secs_f64(step_secs.max(0.0) * 5.0)
            .max(Duration::from_secs_f64(lease_secs.max(0.0) / 2.0)),
        _ => Duration::ZERO,
    }
}

/// Everything the check phase judges from
struct Evidence {
    /// The driver's log, in commit order
    entries: Vec<LogEntry>,
    /// Whether the driver's log was longer than the cap and got cut short
    overflowed: bool,
    /// The leader record the driver's election holds
    snapshot: Option<LeaderRecord>,
    /// The driver election's own history, newest first
    history: Vec<HistoryEvent>,
    /// The elector role's log, in commit order
    elector_entries: Vec<LogEntry>,
    /// Whether the elector's log was longer than the cap and got cut short
    elector_overflowed: bool,
    /// The leader record the elector's election holds
    elector_snapshot: Option<LeaderRecord>,
    /// The elector election's own history, newest first
    elector_history: Vec<HistoryEvent>,
}

/// How many times any check-phase transaction may retry
///
/// The check phase runs on a budget ([`get_check_timeout`]), and the default
/// retry loop has no limit at all: a read that cannot succeed spends the whole
/// budget failing at it and then reports nothing, which looks exactly like a run
/// with nothing to report. A bounded read that gives up says so instead, as an
/// error, and the run fails on it.
///
/// [`get_check_timeout`]: RustWorkload::get_check_timeout
const CHECK_RETRY_LIMIT: i32 = 100;

/// The most log entries the check phase will read from one subspace
///
/// Every client's journal refuses to write past its own ceiling, so the field's
/// worth of ceilings is every entry a run can legitimately have produced.
/// Reading more than that means the bound the journal is supposed to enforce did
/// not hold, and the read stops rather than trying to hold the whole thing in
/// memory: a run that hot-looped has to fail loudly and quickly, not die of
/// exhaustion with nothing said.
fn log_entry_cap(test_duration: Duration, step: Duration, client_count: i32) -> usize {
    let clients = u64::try_from(client_count.max(1)).unwrap_or(1);
    let cap = op_ceiling(test_duration, step).saturating_mul(clients);
    usize::try_from(cap).unwrap_or(usize::MAX)
}

/// Read one versionstamped log subspace whole, in commit order, up to `cap`
///
/// One transaction, deliberately. Paging across several would let the two logs
/// and the two records be read at different instants, and "nothing is writing
/// during the check phase" is an assumption about the simulator that this suite
/// has never verified; a check that quietly depended on it could pass on
/// evidence that did not describe one moment.
///
/// What bounds the read instead is `cap`. The stream stops one entry past it, so
/// a runaway log costs one entry more than the bound rather than however much
/// the run managed to write, which keeps both the memory and the transaction's
/// own five-second limit inside what the cap allows.
///
/// Returns the entries and whether the cap was exceeded. A capped read is not a
/// shorter answer, it is evidence that something wrote more than the run's shape
/// can explain, and the caller fails the run on it.
async fn read_log(
    trx: &RetryableTransaction,
    subspace: &Subspace,
    cap: usize,
) -> Result<(Vec<LogEntry>, bool), FdbBindingError> {
    let (begin, end) = subspace.range();
    let options = RangeOption {
        // One past the cap: reading exactly `cap` cannot distinguish a log that
        // fits from one that was cut off at the boundary.
        limit: Some(cap.saturating_add(1)),
        mode: StreamingMode::WantAll,
        ..RangeOption::from((begin, end))
    };

    let mut entries = Vec::new();
    let mut stream = trx.get_ranges_keyvalues(options, true);
    while let Some(kv) = stream.try_next().await? {
        if entries.len() == cap {
            return Ok((entries, true));
        }
        entries.push(LogEntry::decode(subspace, kv.key(), kv.value()).map_err(decoded)?);
    }
    Ok((entries, false))
}

/// Wrap a decoding failure, keeping the `source()` chain the retry loop reads
fn decoded<E>(error: E) -> FdbBindingError
where
    E: std::error::Error + Send + Sync + 'static,
{
    FdbBindingError::CustomError(Box::new(error))
}

/// The identifier a client claims under, as its journal derives it
fn leader_id_of(client_id: i32) -> String {
    format!("process_{client_id}")
}

impl RustWorkload for LeaderElectionWorkload {
    async fn setup(&mut self, db: SimDatabase) {
        if let Some(problem) = self.config_error.clone() {
            // A configuration this build does not understand would run a
            // different test than the one the file describes, and pass.
            self.context.trace(
                Severity::Error,
                "LeaderElectionConfigInvalid",
                details!["Problem" => problem],
            );
        }

        self.context.trace(
            Severity::Info,
            "LeaderElectionSetup",
            details![
                "Client" => self.client_id,
                "ClientCount" => self.client_count,
                "Role" => self.role.as_str(),
                "LeaseSecs" => self.config.lease.as_duration().as_secs_f64(),
                "StepSecs" => self.config.step.as_secs_f64(),
                "TestDurationSecs" => self.config.test_duration.as_secs_f64(),
                "SafetyMarginSecs" => self.config.safety_margin().as_secs_f64(),
                "ClockSkewMode" => self.config.skew_mode.as_str(),
                "ClockRate" => format!("{:.6}", self.driver.journal().clock().rate()),
                "SharedRandom" => self.context.shared_random_number()
            ],
        );

        // On every client, and deliberately identical on all of them: the draw
        // is a pure function of the shared seed, so a trace where two clients
        // disagree is a run where something reached for state the plan is not
        // allowed to depend on. It is also the reproduction recipe, which is
        // why it is emitted before anything can fail rather than in `check`.
        if let Some(plan) = &self.plan {
            self.context.trace(
                Severity::Info,
                "LeaderElectionSwarmPlan",
                details![
                    "Seed" => plan.seed,
                    "Plan" => plan.describe(),
                    "FeaturesEnabled" => plan.features.enabled(),
                    // Which clients were converted, on every client: the draw is
                    // a pure function of the plan and the field size, so a trace
                    // where two clients disagree is a run that reached for state
                    // the assignment is not allowed to depend on.
                    "Electors" => format!("{:?}", self.electors),
                    "MinAcquisitions" => self.thresholds.min_acquisitions,
                    "MinRenewals" => self.thresholds.min_renewals,
                    "MinObservedIdentities" => self.thresholds.min_observed_identities,
                    "MinRecoveries" => self.thresholds.min_recoveries,
                    "RenewIntervalSecs" => self.thresholds.renew_interval.as_secs_f64()
                ],
            );
        }

        if self.client_id == 0 {
            let key = Subspace::all().subspace(&("le_meta",)).pack(&("config",));
            let value = pack(&(
                self.config.lease.as_nanos(),
                self.client_count,
                self.config.skew_mode.as_str(),
                self.config.test_duration.as_secs_f64(),
            ));
            let written = db
                .run(|trx, _| {
                    let key = key.clone();
                    let value = value.clone();
                    async move {
                        trx.set(&key, &value);
                        Ok::<_, FdbBindingError>(())
                    }
                })
                .await;
            if let Err(error) = written {
                self.context.trace(
                    Severity::WarnAlways,
                    "LeaderElectionMetaWriteFailed",
                    details!["Error" => format!("{error:?}")],
                );
            }
        }

        // Line the clients up: one that started campaigning while the others
        // were still being created would spend the first steps of the run
        // uncontested, which is the least interesting shape a run can have.
        if let Err(error) = self.context.delay(self.config.step).await {
            // Not fatal, and not silent either. This client is about to start
            // its role with delays that do not work, so the run wants to know
            // where the roles that end early came from.
            self.context.trace(
                Severity::WarnAlways,
                "LeaderElectionSetupDelayFailed",
                details![
                    "Client" => self.client_id,
                    "Error" => format!("{error:?}")
                ],
            );
        }
    }

    async fn start(&mut self, db: SimDatabase) {
        self.driver.run(&db).await;

        let counters = self.driver.counters();
        self.context.trace(
            Severity::Info,
            "LeaderElectionStartComplete",
            details![
                "Client" => self.client_id,
                "Role" => self.role.as_str(),
                "Acquisitions" => counters.acquisitions,
                "Renewals" => counters.renewals,
                "Resigns" => counters.resigns,
                "Denials" => counters.denials,
                "Crashes" => counters.crashes,
                "InjectedUnknowns" => counters.injected_unknowns,
                "RecoveriesAdopted" => counters.recoveries_adopted,
                "Superseded" => counters.superseded,
                "HorizonStops" => counters.horizon_stops,
                "FencedApplied" => counters.fenced_applied,
                "FencedRejected" => counters.fenced_rejected,
                "WorkAbandoned" => counters.work_abandoned,
                "MaxObservedSkewSecs" =>
                    self.driver.journal().clock().max_observed_skew().as_secs_f64()
            ],
        );
    }

    async fn check(&mut self, db: SimDatabase) {
        let evidence = self.read_evidence(&db).await;
        let Evidence {
            entries,
            overflowed,
            snapshot,
            history,
            elector_entries,
            elector_overflowed,
            elector_snapshot,
            elector_history,
        } = match evidence {
            Ok(evidence) => evidence,
            Err(error) => {
                // A check phase that cannot read the log cannot pass: silence
                // here is exactly what the previous suite mistook for success.
                self.context.trace(
                    Severity::Error,
                    "LeaderElectionCheckUnreadable",
                    details![
                        "Client" => self.client_id,
                        "Error" => format!("{error:?}")
                    ],
                );
                return;
            }
        };

        let cap = log_entry_cap(
            self.config.test_duration,
            self.config.step,
            self.client_count,
        );
        if overflowed {
            self.report_overflow("driver", entries.len(), cap);
        }
        if elector_overflowed {
            self.report_overflow("elector", elector_entries.len(), cap);
        }

        let replayed = replay(&entries, leader_id_of);
        let expected = snapshot.as_ref().map(expected_record);
        let history: Vec<HistoryEntry> = history.iter().rev().map(history_entry).collect();

        // Zero slack where the clients share one clock; the rate error the
        // configuration admits, and nothing else, where they do not.
        let tolerances = match self.config.skew_mode {
            SkewMode::None => Tolerances::STRICT,
            mode => Tolerances::from_clock_rate_error(
                self.config.lease.as_duration(),
                mode.max_rate_error(),
            ),
        };

        // The three quantities `ProgressMade` judges, reported whether or not
        // it passes: thresholds that nobody can see the distance to are
        // thresholds nobody can set.
        let acquisitions = replayed
            .transitions
            .iter()
            .filter(|transition| transition.kind.is_acquisition())
            .count();
        let renewals = replayed
            .transitions
            .iter()
            .filter(|transition| transition.kind == TransitionKind::Renew)
            .count();
        let steals = replayed
            .transitions
            .iter()
            .filter(|transition| transition.kind == TransitionKind::Steal)
            .count();
        let mut identities: Vec<_> = entries
            .iter()
            .filter(|entry| entry.record.op == OpKind::Observe)
            .filter_map(|entry| entry.record.observed)
            .collect();
        identities.sort_unstable_by_key(|seen| (seen.ballot, seen.generation, seen.vacant));
        identities.dedup();
        // Whether the unknown-commit path ran at all. `UuidRecoveryNoDup` holds
        // vacuously over a run that never lost a commit reply, and a run of
        // zeroes here says the invariant was never actually asked anything.
        // Both resolutions count: an attempt that found a stranger at its own
        // ballot exercised the recovery just as much as one that adopted.
        let recoveries = entries
            .iter()
            .filter(|entry| is_resolution(&entry.record))
            .count();
        let superseded = entries
            .iter()
            .filter(|entry| entry.record.superseded)
            .count();
        let injected = entries
            .iter()
            .filter(|entry| entry.record.op == OpKind::InjectedUnknown)
            .count();

        self.context.trace(
            Severity::Info,
            "LeaderElectionCheckStart",
            details![
                "Client" => self.client_id,
                "LogEntries" => entries.len(),
                "Transitions" => replayed.transitions.len(),
                "Acquisitions" => acquisitions,
                "Steals" => steals,
                "Renewals" => renewals,
                "ObservedIdentities" => identities.len(),
                "Recoveries" => recoveries,
                "InjectedUnknowns" => injected,
                "SupersededResolutions" => superseded,
                "Beliefs" => replayed.beliefs.len(),
                "HistoryEntries" => history.len(),
                "BeliefToleranceMs" => tolerances.belief_overlap.as_millis() as u64,
                "ObservationSlackMs" => tolerances.observation_slack.as_millis() as u64
            ],
        );

        let reports = check_all(&CheckInputs {
            entries: &entries,
            replay: &replayed,
            snapshot: expected.as_ref(),
            history: &history,
            tolerances,
            thresholds: self.thresholds,
        });

        let mut first_violation = None;
        let mut any_failed = false;
        for report in &reports {
            if report.passed() {
                self.context.trace(
                    Severity::Info,
                    "LeaderElectionInvariantHeld",
                    details!["Invariant" => report.name],
                );
                continue;
            }
            any_failed = true;
            first_violation = first_violation.or_else(|| {
                report
                    .violations
                    .first()
                    .and_then(|violation| violation.indices.first().copied())
            });
            self.report_violations(report);
        }

        // Judged separately, and its failures do not steer the dump below:
        // the indices an elector violation names are positions in the merged
        // order of two streams, not offsets into the driver's log.
        self.check_electors(
            &elector_entries,
            elector_snapshot.as_ref(),
            &elector_history,
            tolerances,
        );

        match first_violation {
            Some(index) => self.dump_around(&entries, index),
            // A violation about the run as a whole (ProgressMade) names no
            // entry, so there is nothing to dump *around*. The end of the log
            // is what a reader needs instead: it says what the run was doing
            // when it ran out of time.
            None if any_failed => {
                self.dump_around(&entries, entries.len().saturating_sub(1));
            }
            None => {}
        }
    }

    fn get_metrics(&self, mut out: Metrics) {
        let counters = self.driver.counters();
        out.extend([
            Metric::val("client_id", f64::from(self.client_id)),
            Metric::val("acquisitions", counters.acquisitions as f64),
            Metric::val("renewals", counters.renewals as f64),
            Metric::val("resigns", counters.resigns as f64),
            Metric::val("denials", counters.denials as f64),
            Metric::val("superseded", counters.superseded as f64),
            Metric::val("injected_unknowns", counters.injected_unknowns as f64),
            Metric::val("recoveries_adopted", counters.recoveries_adopted as f64),
            Metric::val("lost", counters.lost as f64),
            Metric::val("crashes", counters.crashes as f64),
            Metric::val("horizon_stops", counters.horizon_stops as f64),
            Metric::val("fenced_applied", counters.fenced_applied as f64),
            Metric::val("fenced_rejected", counters.fenced_rejected as f64),
            Metric::val("work_abandoned", counters.work_abandoned as f64),
            Metric::val("sightings", counters.sightings as f64),
            Metric::val("errors", counters.errors as f64),
            Metric::val("elector_acquisitions", counters.elector_acquisitions as f64),
            Metric::val(
                "elector_fenced_applied",
                counters.elector_fenced_applied as f64,
            ),
            Metric::val(
                "elector_fenced_rejected",
                counters.elector_fenced_rejected as f64,
            ),
            Metric::val("elector_lease_losses", counters.elector_lease_losses as f64),
            Metric::val("elector_resigns", counters.elector_resigns as f64),
            Metric::val("ops_logged", self.driver.journal().ops_logged() as f64),
            // Reported next to the count so a reader can see how close a client
            // came to its ceiling. One that reached it stopped early, and this
            // is where that shows.
            Metric::val("op_ceiling", self.driver.journal().op_ceiling() as f64),
            Metric::val(
                "max_observed_skew_secs",
                self.driver
                    .journal()
                    .clock()
                    .max_observed_skew()
                    .as_secs_f64(),
            ),
        ]);
    }

    fn get_check_timeout(&self) -> f64 {
        // The check reads a log whose size grows with the run, so its budget
        // has to grow with it too.
        self.config.test_duration.as_secs_f64() * 4.0 + 300.0
    }
}

impl LeaderElectionWorkload {
    /// Read everything the check phase judges from, in one retried transaction
    ///
    /// A snapshot read: nothing is writing any more, and taking conflict ranges
    /// over the whole log would only make the read fight itself on a retry.
    ///
    /// One transaction for all six reads, so the two elections are judged at a
    /// single instant: an elector's log read after its record could hold a
    /// belief the record no longer explains. What keeps that affordable is the
    /// entry cap the two log reads carry, not paging.
    async fn read_evidence(&self, db: &SimDatabase) -> Result<Evidence, FdbBindingError> {
        let cap = log_entry_cap(
            self.config.test_duration,
            self.config.step,
            self.client_count,
        );
        let subspace = log_subspace();
        let elector_subspace = elector_log_subspace();
        db.run(|trx, _| {
            let subspace = &subspace;
            let elector_subspace = &elector_subspace;
            async move {
                // The retry loop is otherwise unbounded, and a read that cannot
                // succeed would spend the whole check budget failing at it and
                // then report nothing, which looks exactly like a run with
                // nothing to report.
                trx.set_option(TransactionOption::RetryLimit(CHECK_RETRY_LIMIT))?;

                let (entries, overflowed) = read_log(&trx, subspace, cap).await?;
                let snapshot = self.election.leader(&trx).await.map_err(decoded)?;
                let history = self
                    .election
                    .history(&trx, HISTORY_LIMIT)
                    .await
                    .map_err(decoded)?;

                let (elector_entries, elector_overflowed) =
                    read_log(&trx, elector_subspace, cap).await?;
                let elector_snapshot = self.elector_election.leader(&trx).await.map_err(decoded)?;
                let elector_history = self
                    .elector_election
                    .history(&trx, HISTORY_LIMIT)
                    .await
                    .map_err(decoded)?;

                Ok(Evidence {
                    entries,
                    overflowed,
                    snapshot,
                    history,
                    elector_entries,
                    elector_overflowed,
                    elector_snapshot,
                    elector_history,
                })
            }
        })
        .await
    }

    /// Fail the run because a log was longer than the run could explain
    ///
    /// `Severity::Error`, because a log that overflowed is a run in which some
    /// loop stopped being paced by simulated time. Everything judged below is
    /// judged on a prefix, so a pass would be a pass on evidence that was cut
    /// off, and this failure is the honest reading of the run either way.
    fn report_overflow(&self, which: &str, entries: usize, cap: usize) {
        self.context.trace(
            Severity::Error,
            "LeaderElectionLogOverflow",
            details![
                "Client" => self.client_id,
                "Log" => which,
                "Detail" => format!("log overflow: {entries} entries, cap {cap}")
            ],
        );
    }

    /// Judge the elector half of the run, or say why it was not judged
    ///
    /// A skipped invariant is named with its reason. Silence is what the suite
    /// this replaces mistook for success, and an elector check that quietly
    /// stopped running would be indistinguishable from one that had nothing to
    /// complain about.
    fn check_electors(
        &self,
        entries: &[LogEntry],
        snapshot: Option<&LeaderRecord>,
        history: &[HistoryEvent],
        tolerances: Tolerances,
    ) {
        if self.electors.is_empty() {
            for name in ELECTOR_INVARIANTS {
                self.context.trace(
                    Severity::Info,
                    "LeaderElectionInvariantSkipped",
                    details![
                        "Invariant" => name,
                        "Reason" => self.no_elector_reason()
                    ],
                );
            }
            return;
        }

        let history: Vec<StampedTransition> =
            history.iter().rev().map(stamped_transition).collect();
        let snapshot = snapshot.map(elector_snapshot);
        let replayed = replay(entries, leader_id_of);

        // Retention trims the trail from the front, so part of a long run is
        // simply not evidence. Both numbers say how much: a history that keeps
        // arriving truncated, or a climbing count of writes nobody could judge,
        // means the retention bound is too small for the churn this plan draws.
        let merged = merge(&history, entries);
        self.context.trace(
            Severity::Info,
            "LeaderElectionElectorCheckStart",
            details![
                "Client" => self.client_id,
                "Electors" => format!("{:?}", self.electors),
                "LogEntries" => entries.len(),
                "HistoryEntries" => history.len(),
                "Beliefs" => replayed.beliefs.len(),
                "FirstJudgeableBallot" => format!("{:?}", first_judgeable_ballot(&merged)),
                "WritesOutsideTheWindow" => writes_outside_the_window(&merged)
            ],
        );

        let reports = check_elector(
            &ElectorEvidence {
                history: &history,
                log: entries,
                replay: &replayed,
                snapshot: snapshot.as_ref(),
                tolerances,
                thresholds: ElectorThresholds::ACTIVE,
            },
            leader_id_of,
        );

        for report in &reports {
            if report.passed() {
                self.context.trace(
                    Severity::Info,
                    "LeaderElectionInvariantHeld",
                    details!["Invariant" => report.name],
                );
            } else {
                self.report_violations(report);
            }
        }
    }

    /// Why this run has no elector half to judge
    fn no_elector_reason(&self) -> &'static str {
        match &self.plan {
            None => "an anchor configuration runs no elector",
            Some(plan) if !plan.features.real_elector => {
                "the plan did not draw the realElector feature"
            }
            Some(_) => {
                "the plan drew realElector, but converting two clients would have left \
                 the driver election with fewer than two contenders"
            }
        }
    }

    /// Spell out what broke an invariant
    ///
    /// `Severity::Error` is what fails the run; everything else here is for
    /// whoever reads the trace afterwards.
    fn report_violations(&self, report: &InvariantReport) {
        for violation in report.violations.iter().take(MAX_VIOLATIONS_TRACED) {
            self.context.trace(
                Severity::Error,
                "LeaderElectionInvariantViolated",
                details![
                    "Client" => self.client_id,
                    "Invariant" => report.name,
                    "Detail" => violation.detail,
                    "Entries" => format!("{:?}", violation.indices)
                ],
            );
        }
        if report.violations.len() > MAX_VIOLATIONS_TRACED {
            self.context.trace(
                Severity::WarnAlways,
                "LeaderElectionInvariantViolationsTruncated",
                details![
                    "Invariant" => report.name,
                    "Total" => report.violations.len(),
                    "Traced" => MAX_VIOLATIONS_TRACED
                ],
            );
        }
    }

    /// Dump the log around one point, and only there
    ///
    /// A whole run's log is far too much trace to be useful; the entries either
    /// side of the first failure are what a reader actually needs. A violation
    /// that names no entry passes the end of the log, which is where a run that
    /// simply did not do enough shows what it was doing instead.
    fn dump_around(&self, entries: &[LogEntry], index: usize) {
        if entries.is_empty() {
            // Reachable, and it used to panic here. `ProgressMade` fails a run
            // in which nothing happened at all, and a run in which nothing
            // happened has no log: the violation names no entry, the caller
            // passes the end of an empty log, and the slice below indexed it.
            // There is nothing to dump; the failure has already been reported.
            return;
        }
        // Clamped so the window is always a valid range: an index past the end
        // would otherwise put `first` above `last`.
        let index = index.min(entries.len() - 1);
        let first = index.saturating_sub(DUMP_RADIUS);
        let last = (index + DUMP_RADIUS).min(entries.len() - 1);
        for (offset, entry) in entries[first..=last].iter().enumerate() {
            let record = &entry.record;
            self.context.trace(
                Severity::Info,
                "LeaderElectionLogEntry",
                details![
                    "Index" => first + offset,
                    "Client" => entry.client_id,
                    "Op" => record.op.as_str(),
                    "Applied" => record.outcome.is_applied(),
                    "Ballot" => record.ballot,
                    "Generation" => record.generation,
                    "Wrote" => record.leader_record_written,
                    "RecoveryNoop" => record.recovery_noop,
                    "Observed" => format!("{:?}", record.observed),
                    "LocalNanos" => record.local_nanos,
                    "SimNanos" => record.sim_nanos,
                    "ObservedSince" => format!("{:?}", record.observation_start_nanos),
                    "LeaseNanos" => record.lease_nanos,
                    "HorizonNanos" => record.horizon_nanos
                ],
            );
        }
    }
}

/// The stored record as replay describes it
fn expected_record(record: &LeaderRecord) -> ExpectedRecord {
    ExpectedRecord {
        ballot: record.ballot(),
        generation: record.generation(),
        leader_id: record.leader_id().unwrap_or_default().to_string(),
        token: *record.token().as_bytes(),
        lease_nanos: record.lease().map_or(0, LeaseDuration::as_nanos),
    }
}

/// One entry of the recipe's own audit trail
fn history_entry(event: &HistoryEvent) -> HistoryEntry {
    HistoryEntry {
        kind: history_kind(event.kind()),
        ballot: event.ballot(),
        leader_id: event.leader_id().to_string(),
    }
}

/// The same entry, with the commit version that orders it against the elector
/// role's log
///
/// Only the ten-byte commit version is comparable across transactions; the two
/// bytes of user version order writes inside one transaction, and the recipe's
/// history and the role's log never share one.
fn stamped_transition(event: &HistoryEvent) -> StampedTransition {
    let mut stamp = [0u8; 10];
    stamp.copy_from_slice(&event.versionstamp()[..10]);
    StampedTransition {
        stamp,
        kind: history_kind(event.kind()),
        ballot: event.ballot(),
        leader_id: event.leader_id().to_string(),
    }
}

/// The leader record as the elector invariants describe it
fn elector_snapshot(record: &LeaderRecord) -> ElectorSnapshot {
    ElectorSnapshot {
        ballot: record.ballot(),
        leader_id: record.leader_id().unwrap_or_default().to_string(),
        vacant: record.is_vacant(),
    }
}

/// How the recipe's own event kinds map onto the ones the checks use
fn history_kind(kind: HistoryEventKind) -> HistoryKind {
    match kind {
        HistoryEventKind::Claim => HistoryKind::Claim,
        HistoryEventKind::Steal => HistoryKind::Steal,
        HistoryEventKind::Resign => HistoryKind::Resign,
    }
}
