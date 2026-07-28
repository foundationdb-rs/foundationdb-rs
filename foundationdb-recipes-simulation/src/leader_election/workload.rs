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
//! ignored setting. That is also why all five configurations carry the same
//! knobs even where a value does nothing: an unread knob would fail the run it
//! is irrelevant to.

use std::time::Duration;

use foundationdb::options::StreamingMode;
use foundationdb::recipes::leader_election::{
    HistoryEvent, HistoryEventKind, LeaderElection, LeaderRecord, LeaseDuration,
};
use foundationdb::recipes::ranked_register::RankedRegister;
use foundationdb::tuple::{Subspace, pack};
use foundationdb::{FdbBindingError, RangeOption};
use foundationdb_simulation::{
    Metric, Metrics, RustWorkload, Severity, SimDatabase, SingleRustWorkload, WorkloadContext,
    details,
};
use futures::TryStreamExt;

use super::clock::{SkewMode, SkewedClock};
use super::invariants::{
    CheckInputs, HistoryEntry, HistoryKind, InvariantReport, ProgressThresholds, Tolerances,
    check_all,
};
use super::log_schema::{LogEntry, OpKind, log_subspace};
use super::logged_op::Journal;
use super::replay::{ExpectedRecord, TransitionKind, replay};
use super::roles::{Driver, DriverConfig, Role};

/// How many transition records the check phase asks the recipe for
///
/// Larger than any run produces, so the trail is compared whole unless the
/// recipe's own retention trimmed it, which the invariant allows for.
const HISTORY_LIMIT: usize = 4096;
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
    config: DriverConfig,
    thresholds: ProgressThresholds,
    role: Role,
    driver: Driver,
    /// A configuration this build cannot honour, reported in `setup` where
    /// there is a trace sink to report it to
    config_error: Option<String>,
}

impl SingleRustWorkload for LeaderElectionWorkload {
    fn new(_name: String, context: WorkloadContext) -> Self {
        let client_id = context.client_id();
        let client_count = context.client_count();

        // ------------------------------------------------------------------
        // Every knob of every configuration, read exactly once.
        // ------------------------------------------------------------------
        let lease_secs: f64 = context.get_option("leaseDurationSecs").unwrap_or(10.0);
        let step_secs: f64 = context.get_option("stepIntervalSecs").unwrap_or(1.0);
        let test_duration_secs: f64 = context.get_option("testDurationSecs").unwrap_or(60.0);
        let resign_probability: f64 = context.get_option("resignProbability").unwrap_or(0.1);
        let crash_probability: f64 = context.get_option("crashProbability").unwrap_or(0.0);
        let clock_skew_mode: String = context
            .get_option("clockSkewMode")
            .unwrap_or_else(|| "none".to_string());
        let pause_factor: f64 = context.get_option("pauseFactor").unwrap_or(2.0);
        let sleeper_enabled: bool = context.get_option("sleeperEnabled").unwrap_or(false);
        let min_acquisitions: usize = context.get_option("minLeadershipClaims").unwrap_or(2);
        let min_renewals: usize = context.get_option("minRenewals").unwrap_or(2);
        let min_observed_identities: usize =
            context.get_option("minObservedIdentities").unwrap_or(2);

        let mut config_error = None;
        let skew_mode = SkewMode::parse(&clock_skew_mode).unwrap_or_else(|| {
            config_error = Some(format!(
                "clockSkewMode {clock_skew_mode:?} is not one of none, random, extreme"
            ));
            SkewMode::None
        });
        let lease =
            LeaseDuration::new(Duration::from_secs_f64(lease_secs)).unwrap_or_else(|error| {
                config_error = Some(format!(
                    "leaseDurationSecs {lease_secs} is unusable: {error}"
                ));
                LeaseDuration::new(Duration::from_secs(10)).expect("ten seconds is a valid lease")
            });

        let role = Role::assign(client_id, client_count, sleeper_enabled);
        let config = DriverConfig {
            lease,
            step: Duration::from_secs_f64(step_secs.max(0.0)),
            test_duration: Duration::from_secs_f64(test_duration_secs.max(0.0)),
            resign_probability,
            crash_probability,
            pause_factor,
            skew_mode,
            // Only when a Sleeper was actually assigned: the head start is
            // dead time in every other configuration.
            sleeper_head_start: match Role::assign(1, client_count, sleeper_enabled) {
                // Long enough to cover one slow first commit: under contention
                // an opening claim can take a good fraction of a lease to land,
                // and a head start shorter than that decides nothing.
                Role::Sleeper => Duration::from_secs_f64(step_secs.max(0.0) * 5.0)
                    .max(Duration::from_secs_f64(lease_secs.max(0.0) / 2.0)),
                _ => Duration::ZERO,
            },
        };

        let clock = SkewedClock::new(skew_mode, lease.as_duration(), config.test_duration, || {
            context.rnd()
        });
        let election = LeaderElection::new(Subspace::all().subspace(&("leader_election",)));
        let journal = Journal::new(
            context.clone(),
            clock,
            election.clone(),
            RankedRegister::new(Subspace::all().subspace(&("le_register",))),
            client_id,
        );
        Self {
            driver: Driver::new(context.clone(), journal, config, role),
            context,
            client_id,
            client_count,
            election,
            config,
            thresholds: ProgressThresholds {
                min_acquisitions,
                min_renewals,
                min_observed_identities,
                // Not a knob: it has to be the interval the driver actually
                // renews on (`roles.rs`), or the check would excuse runs that
                // did have the chance to renew.
                renew_interval: lease.as_duration() / 3,
            },
            role,
            config_error,
        }
    }
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
        let _ = self.context.delay(self.config.step).await;
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
        let (entries, snapshot, history) = match evidence {
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

        let replayed = replay(&entries, |client_id| format!("process_{client_id}"));
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
        let recoveries = entries
            .iter()
            .filter(|entry| entry.record.recovery_noop)
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
            Metric::val("lost", counters.lost as f64),
            Metric::val("crashes", counters.crashes as f64),
            Metric::val("horizon_stops", counters.horizon_stops as f64),
            Metric::val("fenced_applied", counters.fenced_applied as f64),
            Metric::val("fenced_rejected", counters.fenced_rejected as f64),
            Metric::val("work_abandoned", counters.work_abandoned as f64),
            Metric::val("sightings", counters.sightings as f64),
            Metric::val("errors", counters.errors as f64),
            Metric::val("ops_logged", self.driver.journal().ops_logged() as f64),
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
    async fn read_evidence(
        &self,
        db: &SimDatabase,
    ) -> Result<(Vec<LogEntry>, Option<LeaderRecord>, Vec<HistoryEvent>), FdbBindingError> {
        let subspace = log_subspace();
        db.run(|trx, _| {
            let subspace = &subspace;
            async move {
                let (begin, end) = subspace.range();
                let options = RangeOption {
                    mode: StreamingMode::WantAll,
                    ..RangeOption::from((begin, end))
                };

                let mut entries = Vec::new();
                let mut stream = trx.get_ranges_keyvalues(options, true);
                while let Some(kv) = stream.try_next().await? {
                    entries.push(
                        LogEntry::decode(subspace, kv.key(), kv.value())
                            .map_err(|error| FdbBindingError::CustomError(Box::new(error)))?,
                    );
                }

                let snapshot = self
                    .election
                    .leader(&trx)
                    .await
                    .map_err(|error| FdbBindingError::CustomError(Box::new(error)))?;
                let history = self
                    .election
                    .history(&trx, HISTORY_LIMIT)
                    .await
                    .map_err(|error| FdbBindingError::CustomError(Box::new(error)))?;

                Ok((entries, snapshot, history))
            }
        })
        .await
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
        let first = index.saturating_sub(DUMP_RADIUS);
        let last = (index + DUMP_RADIUS).min(entries.len().saturating_sub(1));
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
        kind: match event.kind() {
            HistoryEventKind::Claim => HistoryKind::Claim,
            HistoryEventKind::Steal => HistoryKind::Steal,
            HistoryEventKind::Resign => HistoryKind::Resign,
        },
        ballot: event.ballot(),
        leader_id: event.leader_id().to_string(),
    }
}
