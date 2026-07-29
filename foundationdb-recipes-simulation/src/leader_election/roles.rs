//! What each client spends the run doing.
//!
//! The workload drives the recipe's transaction-level primitives, so the state
//! machine the async handle implements (campaign, renew, hard-stop at the
//! belief horizon, resign) is emulated here on simulated time. That is
//! deliberate: the primitives are pure functions of caller-supplied time, which
//! is exactly what a deterministic simulator can drive, and emulating the loop
//! is what lets the driver log the belief transitions no in-transaction
//! timestamp can reconstruct.
//!
//! # Belief, and when it is recorded
//!
//! A client believes it leads from the moment its claim commits until its
//! *horizon*, which is one lease after the claim's pre-issuance anchor, minus a
//! safety margin covering the clock error the configuration admits. Two rules
//! keep the record of that honest:
//!
//! - the belief-end is logged *before* the resign that follows it commits, so a
//!   clean handover cannot read as an overlap;
//! - a belief-end is never logged once the horizon has passed. There is nothing
//!   to say: the horizon already ended the belief, and a record written later
//!   would claim the client believed longer than it was entitled to. A crashed
//!   or paused leader takes exactly this path, which is why it needs no special
//!   case.
//!
//! # Roles
//!
//! Contenders campaign, renew and resign. One Sleeper reproduces the Kleppmann
//! pause: it takes a term, stops responding for longer than its lease, and only
//! once a successor has demonstrably taken over *and* written under a higher
//! rank does it try to use its stale term. One Watcher discovers leadership
//! through the term key rather than by polling.
//!
//! Every term arrives with its fence already installed, because
//! [`Journal::claim`](super::logged_op::Journal::claim) installs it in the claim
//! transaction. A leader therefore never has fenced work to do before it is able
//! to refuse its predecessor's.
//!
//! # Forced recovery
//!
//! A contender may also throw away a claim reply it did receive, wait, and then
//! re-run the same attempt. That is the only way the recipe's unknown-commit
//! recovery is reached under simulation, and it is a driver-level injection
//! precisely so that the recipe and the log's own idempotency stay untouched:
//! see [`ForcedRecoveryConfig`].
//!
//! Every role logs sightings of the leader record, not just the Watcher. That
//! is the failover story under attrition: the liveness check needs somebody to
//! have been watching, and any survivor will do.

use std::time::Duration;

use foundationdb::FdbBindingError;
use foundationdb::env::Rng;
use foundationdb::recipes::leader_election::{
    ClaimAttempt, ClaimOutcome, ClaimToken, LeaseDuration, LeaseGrant, LeaseObservation,
    RefreshAttempt, RefreshOutcome, ResignOutcome,
};
use foundationdb::recipes::ranked_register::WriteResult;
use foundationdb_simulation::{Severity, SimDatabase, WorkloadContext, details};
use futures::future::Either;

use super::clock::SkewMode;
use super::logged_op::Journal;
use super::swarm::FaultTiming;

/// What a client does for the length of the run
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    /// Campaigns, renews and resigns
    Contender,
    /// Takes a term, pauses past its lease, then must be fenced out
    Sleeper,
    /// Follows leadership through the term key
    Watcher,
}

impl Role {
    /// Assign a role from the client's position in the run
    ///
    /// Roles degrade gracefully: a run with one or two clients is all
    /// contenders, since a Sleeper with nobody to take over from it and a
    /// Watcher with nothing to watch only remove contention.
    ///
    /// Both special roles are gated on their feature as well as on the field
    /// size, because a run that draws neither of them wants the client back as
    /// a contender rather than idle: a Watcher never campaigns, so leaving one
    /// in place would quietly shrink the field a feature-free run contends
    /// with.
    pub(crate) fn assign(
        client_id: i32,
        client_count: i32,
        sleeper_enabled: bool,
        watcher_enabled: bool,
    ) -> Self {
        if sleeper_enabled && client_count >= 3 && client_id == 1 {
            Self::Sleeper
        } else if watcher_enabled && client_count >= 4 && client_id == 2 {
            Self::Watcher
        } else {
            Self::Contender
        }
    }

    /// The name this role appears under in the trace
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Contender => "contender",
            Self::Sleeper => "sleeper",
            Self::Watcher => "watcher",
        }
    }
}

/// When the driver throws a claim reply away on purpose
///
/// The BUGGIFY-style injection. The recipe's recovery path is reached when a
/// claim commits and its caller never learns that it did; under simulation that
/// almost never happens by itself, because every logged transaction sets
/// `AutomaticIdempotency` and the client resolves the unknown commit before the
/// recipe is asked anything. Rather than weaken the log's idempotency to
/// provoke it, the driver simulates the lost reply one layer up: it drops a
/// reply it did receive, starts believing nothing, and re-runs the *same*
/// attempt later. Everything the recipe sees is what it would have seen had the
/// reply really been lost.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ForcedRecoveryConfig {
    /// Whether the run may inject at all
    pub(crate) enabled: bool,
    /// The chance of injecting on a winning claim, after the first
    ///
    /// The first winning claim of every contender injects unconditionally, so
    /// that a run which drew the feature exercises it rather than hoping to.
    pub(crate) probability: f64,
    /// The longest a dropped reply is left unresolved, in leases
    pub(crate) max_delay_leases: f64,
}

impl ForcedRecoveryConfig {
    /// The configuration of a run that never injects
    pub(crate) fn disabled() -> Self {
        Self {
            enabled: false,
            probability: 0.0,
            max_delay_leases: 0.0,
        }
    }
}

/// A claim whose reply the driver threw away, and when it will be re-run
///
/// The attempt is the whole of the recovery state: it carries the token and the
/// ballot its first execution issued, which is what makes the re-run take the
/// recipe's `AlreadyWon` or `Superseded` path instead of claiming again.
struct PendingUnknown {
    attempt: ClaimAttempt,
    attempt_id: u64,
    /// When the re-probe is due, on this client's own clock
    resume_at: Duration,
}

/// The timings and probabilities a run is configured with
#[derive(Debug, Clone)]
pub(crate) struct DriverConfig {
    /// The lease every claim advertises
    pub(crate) lease: LeaseDuration,
    /// How long a client waits between actions
    pub(crate) step: Duration,
    /// How long the start phase runs, in simulated time
    pub(crate) test_duration: Duration,
    /// When a leader hands its term back
    pub(crate) resign: FaultTiming,
    /// When a leader stops responding for longer than its lease
    pub(crate) crash: FaultTiming,
    /// How many leases the Sleeper pauses for
    pub(crate) pause_factor: f64,
    /// How long the other roles hold back so the Sleeper can take the first
    /// term
    ///
    /// The pause scenario needs the Sleeper to actually lead, and one client in
    /// a field of eight wins the opening race about one time in eight. Every
    /// client computes the same role assignment, so each one can decide on its
    /// own to let the Sleeper go first; nothing is coordinated through the
    /// database.
    pub(crate) sleeper_head_start: Duration,
    /// What the clocks are allowed to do
    pub(crate) skew_mode: SkewMode,
    /// When a claim reply is thrown away on purpose
    pub(crate) forced_recovery: ForcedRecoveryConfig,
}

impl DriverConfig {
    /// How long after a term is anchored its renewal comes due
    ///
    /// Two renewals per lease, so losing one transaction is not fatal. The
    /// check phase is told this number so that `ProgressMade` can tell a run
    /// that never had the chance to renew from one that had the chance and did
    /// not take it.
    pub(crate) fn renew_interval(&self) -> Duration {
        self.lease.as_duration() / 3
    }

    /// How much of every lease is given up so that belief cannot overlap
    ///
    /// The rate term is the one that makes the horizon safe, and it is the same
    /// formula the check phase derives its tolerance from. The tenth of a lease
    /// on top covers what is not clock error at all: this driver polls, so it
    /// notices the horizon at its next step rather than the instant it passes,
    /// and a commit takes simulated time the client's own timestamps do not
    /// see.
    pub(crate) fn safety_margin(&self) -> Duration {
        let error = self.skew_mode.max_rate_error();
        let lease = self.lease.as_duration();
        lease.mul_f64(2.0 * error / (1.0 + error)) + lease / 10
    }
}

/// What a run achieved, reported as simulation metrics
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Counters {
    /// Applied claims and steals
    pub(crate) acquisitions: u64,
    /// Applied renewals
    pub(crate) renewals: u64,
    /// Applied resigns
    pub(crate) resigns: u64,
    /// Claims refused because the current term had not been still long enough
    pub(crate) denials: u64,
    /// Campaigns retired because a write of theirs may have committed
    pub(crate) superseded: u64,
    /// Claim replies thrown away on purpose
    pub(crate) injected_unknowns: u64,
    /// Re-probes that found their own record and adopted it
    pub(crate) recoveries_adopted: u64,
    /// Terms lost to a successor
    pub(crate) lost: u64,
    /// Simulated crashes
    pub(crate) crashes: u64,
    /// Terms abandoned because the belief horizon passed
    pub(crate) horizon_stops: u64,
    /// Ranked-register writes that committed
    pub(crate) fenced_applied: u64,
    /// Ranked-register writes the fence refused
    pub(crate) fenced_rejected: u64,
    /// Leader-record sightings
    pub(crate) sightings: u64,
    /// Fenced writes given up because the belief horizon arrived first
    pub(crate) work_abandoned: u64,
    /// Operations that failed with something other than a protocol refusal
    pub(crate) errors: u64,
}

/// One leadership interval, as the driver tracks it
#[derive(Debug)]
struct Term {
    grant: LeaseGrant,
    /// When this client must stop believing, on its own clock
    horizon: Duration,
    /// When the next renewal comes due, on its own clock
    renew_due: Duration,
    /// The fencing sequence used so far within this term
    sequence: u32,
}

/// One client, playing its role
pub(crate) struct Driver {
    context: WorkloadContext,
    journal: Journal,
    config: DriverConfig,
    role: Role,
    /// How long this client has watched the record hold still; threaded through
    /// every campaign, because it is the only thing that ever authorizes a
    /// steal
    observation: LeaseObservation,
    attempt_id: u64,
    /// A claim whose reply this client threw away, waiting to be re-run
    pending_unknown: Option<PendingUnknown>,
    /// Whether this client has already forced one recovery
    injected_once: bool,
    /// True simulated time at which this client entered the start phase
    start_sim: Duration,
    deadline: Duration,
    counters: Counters,
}

impl Driver {
    /// Set a client up for its role
    pub(crate) fn new(
        context: WorkloadContext,
        journal: Journal,
        config: DriverConfig,
        role: Role,
    ) -> Self {
        Self {
            context,
            journal,
            config,
            role,
            observation: LeaseObservation::new(),
            attempt_id: 0,
            pending_unknown: None,
            injected_once: false,
            start_sim: Duration::ZERO,
            deadline: Duration::ZERO,
            counters: Counters::default(),
        }
    }

    /// The instrumented recipe this driver acts through
    pub(crate) fn journal(&self) -> &Journal {
        &self.journal
    }

    /// What this client achieved
    pub(crate) fn counters(&self) -> Counters {
        self.counters
    }

    /// Play the role until the run's simulated deadline
    ///
    /// An infrastructure error ends this client's participation and is traced,
    /// but does not by itself fail the run: what a client failed to do shows up
    /// in the check phase as missing progress, which is a judgement the whole
    /// log gets to make rather than one client's error handling.
    pub(crate) async fn run(&mut self, db: &SimDatabase) {
        self.start_sim = self.journal.sim_now();
        self.deadline = self.start_sim + self.config.test_duration;

        if self.role != Role::Sleeper && !self.config.sleeper_head_start.is_zero() {
            self.delay(self.config.sleeper_head_start).await;
        }

        let outcome = match self.role {
            Role::Watcher => self.watch(db).await,
            Role::Sleeper => self.pause_and_be_fenced(db).await,
            Role::Contender => self.contend(db).await,
        };

        if let Err(error) = outcome {
            self.counters.errors += 1;
            self.trace(
                Severity::WarnAlways,
                "LeaderElectionRoleFailed",
                details![
                    "Role" => self.role.as_str(),
                    "Error" => format!("{error:?}")
                ],
            );
        }
    }

    // ========================================================================
    // CONTENDER
    // ========================================================================

    async fn contend(&mut self, db: &SimDatabase) -> Result<(), FdbBindingError> {
        let mut term: Option<Term> = None;
        let mut step = 0u64;

        while self.journal.sim_now() < self.deadline {
            term = match term {
                None => self.campaign(db).await?,
                Some(held) => self.serve(db, held).await?,
            };
            // Every role watches, so that killing the Watcher cannot make the
            // run look like nobody was ever elected. Every other step is
            // enough for that, and a transaction not issued is a transaction
            // not competing with the ones that matter.
            if step % 2 == 0 {
                self.sight(db, false).await?;
            }
            step += 1;
            match &term {
                // A leader waits until its renewal is due rather than a whole
                // step past it. With a short lease a step is a large fraction
                // of the term, and overshooting the deadline is how a leader
                // ends up at its horizon having never renewed.
                Some(held) => self.pace_until(held.renew_due).await,
                None => self.pace().await,
            }
        }

        if let Some(held) = term {
            self.step_down(db, held).await?;
        }
        Ok(())
    }

    /// One campaign transaction
    ///
    /// A fresh attempt (and so a fresh token) per transaction: the attempt
    /// anchors the lease just before the write is issued, so reusing one across
    /// a long campaign would hand back a term that had already spent most of
    /// its life waiting to be won.
    ///
    /// The one exception is a reply this client threw away
    /// ([`ForcedRecoveryConfig`]). That attempt is deliberately kept and re-run
    /// once its delay is up, which is the only way the recipe's recovery path
    /// is reached: everything it needs to recognize its own record is in the
    /// attempt, so a re-run under the same one takes `AlreadyWon` or
    /// `Superseded` where a fresh one would claim a second time.
    async fn campaign(&mut self, db: &SimDatabase) -> Result<Option<Term>, FdbBindingError> {
        let (attempt, attempt_id, resumed) = match self.pending_unknown.take() {
            // Still waiting: hand the step back, the caller paces it. Nothing
            // else may run under this client while a claim of its own is
            // unaccounted for, which is what makes the injection the same shape
            // as a client that stopped responding.
            Some(pending) if self.journal.local_now() < pending.resume_at => {
                self.pending_unknown = Some(pending);
                return Ok(None);
            }
            Some(pending) => (pending.attempt, pending.attempt_id, true),
            None => {
                let attempt = ClaimAttempt::new(self.token(), self.journal.local_now())
                    .map_err(|error| FdbBindingError::CustomError(Box::new(error)))?;
                self.attempt_id += 1;
                (attempt, self.attempt_id, false)
            }
        };

        let (outcome, observation) = self
            .journal
            .claim(
                db,
                self.config.lease,
                &attempt,
                attempt_id,
                self.observation,
            )
            .await?;
        self.observation = observation;

        match outcome {
            ClaimOutcome::Won(grant) => {
                self.counters.acquisitions += 1;
                if resumed {
                    // The re-probe found our own record and adopted it, which
                    // is the whole of the recovery contract's happy path. The
                    // grant it hands back is anchored at the *original*
                    // attempt, so the horizon below is the one the first
                    // execution earned and not a fresh lease.
                    self.counters.recoveries_adopted += 1;
                }
                let horizon = self.horizon_of(&grant);

                // A claim that took longer to commit than the lease it asked
                // for comes back already expired. The term exists in the
                // database, but this client may never act on it: believing for
                // even one step would overlap whoever waits it out.
                if self.journal.local_now() >= horizon {
                    self.counters.horizon_stops += 1;
                    self.trace(
                        Severity::Warn,
                        "LeaderElectionClaimOutlivedItsLease",
                        details!["Ballot" => grant.ballot()],
                    );
                    return Ok(None);
                }

                if !resumed && self.should_drop_reply() {
                    return self.drop_reply(db, attempt, attempt_id, &grant).await;
                }

                self.journal.belief_begin(db, &grant, horizon).await?;
                // The fence was installed by the claim transaction itself, so
                // the term starts already able to refuse its predecessor.
                Ok(Some(Term {
                    renew_due: grant.acquired_at() + self.config.renew_interval(),
                    horizon,
                    sequence: 0,
                    grant,
                }))
            }
            ClaimOutcome::Denied { .. } => {
                // No extra backoff: the caller paces every step, and re-reading
                // more often than the lease is free. A denial does not restart
                // the observation timer, so the polls between one sighting and
                // the steal it earns are what keeps the window alive.
                self.counters.denials += 1;
                Ok(None)
            }
            ClaimOutcome::Superseded => {
                self.counters.superseded += 1;
                self.trace(
                    Severity::Warn,
                    "LeaderElectionAttemptSuperseded",
                    details!["Client" => self.journal.leader_id()],
                );
                Ok(None)
            }
        }
    }

    /// Throw a winning claim's reply away and arrange to re-run the attempt
    ///
    /// Nothing else happens under this term. No belief is recorded and no
    /// fenced work is done, because a client that never heard back does not
    /// know it leads: what the rest of the run sees is a claim that landed and
    /// a client that went quiet, which is the same shape a crash has. The
    /// marker is written after the claim committed, so it exists exactly when
    /// the claim it describes does.
    async fn drop_reply(
        &mut self,
        db: &SimDatabase,
        attempt: ClaimAttempt,
        attempt_id: u64,
        grant: &LeaseGrant,
    ) -> Result<Option<Term>, FdbBindingError> {
        let delay = self.draw_resume_delay(self.counters.injected_unknowns == 0);
        self.counters.injected_unknowns += 1;
        self.journal
            .injected_unknown(db, attempt_id, &attempt, grant.ballot())
            .await?;

        let resume_at = self.journal.local_now() + delay;
        self.trace(
            Severity::Info,
            "LeaderElectionReplyDropped",
            details![
                "Ballot" => grant.ballot(),
                "ResumeAtSecs" => resume_at.as_secs_f64()
            ],
        );
        self.pending_unknown = Some(PendingUnknown {
            attempt,
            attempt_id,
            resume_at,
        });
        Ok(None)
    }

    /// Whether this winning claim's reply should be thrown away
    ///
    /// The first one of a contender is unconditional, so that a run which drew
    /// the feature exercises it rather than hoping to. After that it is rare:
    /// a client that spends the run recovering never gets far enough to be
    /// stolen from, and the terms it abandons are the interesting part.
    ///
    /// The window check is what keeps the check phase honest rather than
    /// flaky. An injection nobody resolved before the deadline is
    /// indistinguishable from a recovery path that quietly stopped working, and
    /// `RecoveryExercised` is entitled to treat it as the latter, so a reply is
    /// only dropped while the run still has room for the longest delay this
    /// configuration can draw and the campaign that follows it.
    fn should_drop_reply(&mut self) -> bool {
        let forced = self.config.forced_recovery;
        if !forced.enabled || self.role != Role::Contender {
            return false;
        }

        // Drawn on every winning claim of an enabled run, whatever the answer
        // is then used for. The same discipline as [`chance`](Self::chance):
        // the sequence a client consumes must not depend on how far into the
        // run it is, or a run would stop replaying from the step the window
        // closed.
        let roll = self.chance(forced.probability);
        if self.injected_once && !roll {
            return false;
        }

        let longest = self
            .config
            .lease
            .as_duration()
            .mul_f64(forced.max_delay_leases + 1.0);
        if self.elapsed_sim() + longest >= self.config.test_duration {
            return false;
        }

        self.injected_once = true;
        true
    }

    /// How long a dropped reply is left unresolved, on this client's own clock
    ///
    /// A client's first injection resumes after one jittered step, which is a
    /// small fraction of a lease: nobody can have timed the record out in that
    /// window, so the re-probe is certain to find its own record and take the
    /// adoption path. Later ones spread out over a lease and a half, which is
    /// well past the point where a contender may have stolen the term, and is
    /// how the terminal half of the contract gets reached.
    fn draw_resume_delay(&self, first: bool) -> Duration {
        if first {
            return self.jittered_step();
        }
        let longest = self
            .config
            .lease
            .as_duration()
            .mul_f64(self.config.forced_recovery.max_delay_leases);
        let step = self.config.step;
        match longest.checked_sub(step) {
            Some(spread) if !spread.is_zero() => {
                let unit = f64::from(self.rng().next_u32()) / f64::from(u32::MAX);
                step + spread.mul_f64(unit)
            }
            _ => step,
        }
    }

    /// One step of holding a term
    async fn serve(
        &mut self,
        db: &SimDatabase,
        mut term: Term,
    ) -> Result<Option<Term>, FdbBindingError> {
        let now = self.journal.local_now();

        // The hard stop. Nothing else in this function gets to run past it.
        if now >= term.horizon {
            self.counters.horizon_stops += 1;
            return Ok(None);
        }

        // The renewal comes first, and it comes first for a reason. A renewal
        // is due at a deadline; ending the term is an event that happens
        // whenever it happens. If this step finds the deadline already behind
        // it, then in real time the renewal timer fired before whatever ends
        // the term, and the handle layer would have renewed: its renewal driver
        // is a future racing the work, not a step that the work gets to
        // preempt. Rolling for a crash or a resign first, as this loop used to,
        // silently skipped renewals that were already owed, which is exactly
        // what `ProgressMade` reports as a belief interval that outlived its
        // renewal deadline without renewing.
        if now >= term.renew_due {
            let attempt = RefreshAttempt::new(&term.grant, self.journal.local_now());
            match self.journal.refresh(db, &term.grant, &attempt).await? {
                RefreshOutcome::Refreshed(grant) => {
                    self.counters.renewals += 1;
                    let horizon = self.horizon_of(&grant);
                    // A renewal that came back after the horizon is discarded:
                    // by then a contender's observation window may already have
                    // started, and no reply can undo that.
                    if self.journal.local_now() >= horizon {
                        self.counters.horizon_stops += 1;
                        return Ok(None);
                    }
                    term.renew_due = grant.acquired_at() + self.config.renew_interval();
                    term.horizon = horizon;
                    term.grant = grant;
                    self.journal.belief_begin(db, &term.grant, horizon).await?;
                    return Ok(Some(term));
                }
                RefreshOutcome::Lost { .. } => {
                    self.counters.lost += 1;
                    self.end_belief(db, &term).await?;
                    return Ok(None);
                }
            }
        }

        if self.chance(self.config.crash.probability_at(self.elapsed_sim())) {
            self.counters.crashes += 1;
            self.trace(
                Severity::Info,
                "LeaderElectionSoftCrash",
                details!["Ballot" => term.grant.ballot()],
            );
            // One long delay past the lease: no renewals, no belief end, and a
            // record left standing still for a successor to time out.
            self.delay(self.config.lease.as_duration().mul_f64(1.5))
                .await;
            return Ok(None);
        }

        if self.chance(self.config.resign.probability_at(self.elapsed_sim())) {
            self.step_down(db, term).await?;
            return Ok(None);
        }

        // Work done under the term, fenced by its ballot.
        self.fence(db, &mut term).await?;
        Ok(Some(term))
    }

    /// Stop believing, then hand the term back
    async fn step_down(&mut self, db: &SimDatabase, term: Term) -> Result<(), FdbBindingError> {
        self.end_belief(db, &term).await?;
        match self.journal.resign(db, &term.grant).await? {
            ResignOutcome::Resigned => self.counters.resigns += 1,
            ResignOutcome::NotHolder => self.counters.lost += 1,
        }
        Ok(())
    }

    // ========================================================================
    // SLEEPER
    // ========================================================================

    /// The Kleppmann pause, barriered so it tests what it claims to
    ///
    /// The stale operations are only attempted once a successor has *both*
    /// taken the term and committed a write under a higher rank. Without that
    /// barrier, a rejected stale write proves nothing: it could have been
    /// refused because no fence had been installed yet, or because the term had
    /// not actually moved.
    async fn pause_and_be_fenced(&mut self, db: &SimDatabase) -> Result<(), FdbBindingError> {
        let term = loop {
            if self.journal.sim_now() >= self.deadline {
                self.trace(
                    Severity::Info,
                    "LeaderElectionSleeperNeverLed",
                    details!["Deadline" => self.deadline.as_secs_f64()],
                );
                return Ok(());
            }
            match self.campaign(db).await? {
                Some(term) => break term,
                None => self.pace().await,
            }
        };

        let pause = self
            .config
            .lease
            .as_duration()
            .mul_f64(self.config.pause_factor);
        self.trace(
            Severity::Info,
            "LeaderElectionSleeperPausing",
            details![
                "Ballot" => term.grant.ballot(),
                "PauseSecs" => pause.as_secs_f64()
            ],
        );
        self.delay(pause).await;

        if !self.wait_for_successor(db, &term).await? {
            self.trace(
                Severity::Info,
                "LeaderElectionSleeperBarrierUnmet",
                details!["Ballot" => term.grant.ballot()],
            );
            return self.contend(db).await;
        }

        // The stale term, used as if nothing had happened. Both operations must
        // be refused; a violation is caught in the check phase rather than
        // here, so that the log is what judges the run.
        let stale_rank = term.grant.rank(term.sequence + 1);
        let write = self
            .journal
            .fenced_write(db, term.grant.ballot(), stale_rank, term.sequence + 1)
            .await?;
        let attempt = RefreshAttempt::new(&term.grant, self.journal.local_now());
        let refresh = self.journal.refresh(db, &term.grant, &attempt).await?;
        self.count_fenced(write);
        if matches!(refresh, RefreshOutcome::Refreshed(_)) {
            self.counters.renewals += 1;
        }

        self.trace(
            Severity::Info,
            "LeaderElectionSleeperWokeUp",
            details![
                "Ballot" => term.grant.ballot(),
                "StaleWriteCommitted" => write.is_committed(),
                "StaleRenewalApplied" => matches!(refresh, RefreshOutcome::Refreshed(_))
            ],
        );

        self.contend(db).await
    }

    /// Wait until somebody else holds the term *and* has written under it
    ///
    /// Returns `false` if the run ends first, which is a scenario that did not
    /// happen rather than one that failed.
    async fn wait_for_successor(
        &mut self,
        db: &SimDatabase,
        term: &Term,
    ) -> Result<bool, FdbBindingError> {
        while self.journal.sim_now() < self.deadline {
            let stolen = self
                .sight(db, false)
                .await?
                .is_some_and(|ballot| ballot > term.grant.ballot());
            let fenced = self
                .journal
                .register_writer(db)
                .await?
                .is_some_and(|(_, ballot, _)| ballot > term.grant.ballot());
            if stolen && fenced {
                return Ok(true);
            }
            self.pace().await;
        }
        Ok(false)
    }

    // ========================================================================
    // WATCHER
    // ========================================================================

    /// Follow leadership through the term key
    ///
    /// The watch is a hint and nothing more: it can coalesce, and a term that
    /// flaps back to its previous holder may produce no wake-up at all. Every
    /// pass re-reads and logs what it saw, whether or not anything changed.
    async fn watch(&mut self, db: &SimDatabase) -> Result<(), FdbBindingError> {
        while self.journal.sim_now() < self.deadline {
            let (_, watch) = self.journal.observe(db, true).await?;
            self.counters.sightings += 1;

            match watch {
                Some(watch) => {
                    let timeout = Box::pin(self.context.delay(self.config.step * 4));
                    let _ = futures::future::select(watch, timeout).await;
                }
                None => self.pace().await,
            }
        }
        Ok(())
    }

    // ========================================================================
    // SHARED
    // ========================================================================

    /// Read and log the current leader record, returning its ballot
    async fn sight(
        &mut self,
        db: &SimDatabase,
        arm_watch: bool,
    ) -> Result<Option<u64>, FdbBindingError> {
        let (current, _) = self.journal.observe(db, arm_watch).await?;
        self.counters.sightings += 1;
        Ok(current.map(|record| record.ballot()))
    }

    /// Do one piece of fenced work, and give it up at the horizon
    ///
    /// The horizon is a hard stop for work, not only for renewals: a write that
    /// keeps retrying past it is a leader acting on a term it has stopped
    /// believing in. The handle layer enforces this by dropping the work future
    /// when the horizon wins its race; dropping the transaction here is the
    /// same thing, and the reason the driver has to race rather than await is
    /// that a retry loop has no idea what time it is.
    async fn fence(&mut self, db: &SimDatabase, term: &mut Term) -> Result<(), FdbBindingError> {
        let remaining = term.horizon.saturating_sub(self.journal.local_now());
        if remaining.is_zero() {
            return Ok(());
        }

        term.sequence += 1;
        // Scoped so the pinned transaction is dropped before the counters are
        // touched: it borrows the journal for as long as it lives.
        let outcome = {
            let work = self.journal.fenced_write(
                db,
                term.grant.ballot(),
                term.grant.rank(term.sequence),
                term.sequence,
            );
            futures::pin_mut!(work);
            let horizon = Box::pin(self.context.delay(remaining));

            match futures::future::select(work, horizon).await {
                Either::Left((outcome, _)) => Some(outcome?),
                // The transaction is dropped with this frame. It may still land
                // (nothing can un-issue a commit), which is exactly why the
                // fence, and not the horizon, is what makes the write safe.
                Either::Right(_) => None,
            }
        };
        match outcome {
            Some(outcome) => self.count_fenced(outcome),
            None => self.counters.work_abandoned += 1,
        }
        Ok(())
    }

    /// Log the end of a belief, unless the horizon already ended it
    async fn end_belief(&self, db: &SimDatabase, term: &Term) -> Result<(), FdbBindingError> {
        if self.journal.local_now() < term.horizon {
            self.journal.belief_end(db, &term.grant).await?;
        }
        Ok(())
    }

    fn count_fenced(&mut self, outcome: WriteResult) {
        if outcome.is_committed() {
            self.counters.fenced_applied += 1;
        } else {
            self.counters.fenced_rejected += 1;
        }
    }

    fn horizon_of(&self, grant: &LeaseGrant) -> Duration {
        grant
            .acquired_at()
            .saturating_add(self.config.lease.as_duration())
            .saturating_sub(self.config.safety_margin())
    }

    /// How long this client has been in the start phase, in true simulated time
    ///
    /// The fault windows are drawn against the run's own timeline, so this is
    /// measured on the undistorted clock: a storm has to mean the same span to
    /// every client, whatever their own clock thinks the time is.
    fn elapsed_sim(&self) -> Duration {
        self.journal.sim_now().saturating_sub(self.start_sim)
    }

    /// The generator every per-client draw comes from
    ///
    /// The environment's, which under simulation is the simulator's own, so the
    /// draws are part of the run's reproducible state.
    fn rng(&self) -> &dyn Rng {
        self.journal.env().rng().as_ref()
    }

    /// A per-term token from the simulator's own generator, so a run replays
    fn token(&self) -> ClaimToken {
        let mut bytes = [0u8; 16];
        for chunk in bytes.chunks_mut(4) {
            chunk.copy_from_slice(&self.rng().next_u32().to_be_bytes());
        }
        if bytes == [0u8; 16] {
            // The all-zero token is the vacancy sentinel and is refused.
            bytes[0] = 1;
        }
        ClaimToken::from_bytes(bytes)
    }

    /// Roll against `probability`
    ///
    /// The draw happens whatever the probability is, including zero: the
    /// sequence a run consumes must not depend on which fault windows happen to
    /// be open, or a run would replay differently from the step a storm ended.
    fn chance(&self, probability: f64) -> bool {
        f64::from(self.rng().next_u32()) / f64::from(u32::MAX) < probability
    }

    async fn delay(&self, duration: Duration) {
        let _ = self.context.delay(duration).await;
    }

    /// Wait out a step, jittered
    ///
    /// Clients that campaign in lockstep spend the run losing conflicts to each
    /// other: every one of them reads the leader key, one writes it, and the
    /// rest retire their attempts and start over. Spreading the steps is the
    /// standard herd avoidance, and it is what lets a run of seven contenders
    /// produce leadership changes rather than contention.
    async fn pace(&self) {
        self.delay(self.jittered_step()).await;
    }

    /// Wait out a step, or up to `due`, whichever comes first
    ///
    /// The renewal driver of the handle layer sleeps exactly until the renewal
    /// is due. This is that, with the step as a ceiling so a leader with a long
    /// lease still takes its other actions in between.
    async fn pace_until(&self, due: Duration) {
        let wait = due
            .saturating_sub(self.journal.local_now())
            .min(self.jittered_step());
        if !wait.is_zero() {
            self.delay(wait).await;
        }
    }

    fn jittered_step(&self) -> Duration {
        let spread = 0.5 + f64::from(self.rng().next_u32()) / f64::from(u32::MAX);
        self.config.step.mul_f64(spread)
    }

    fn trace<S2, S3>(&self, severity: Severity, name: &str, details: &[(S2, S3)])
    where
        S2: AsRef<str>,
        S3: AsRef<str>,
    {
        self.context.trace(severity, name, details);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watcher_gate_turns_client_two_into_a_contender() {
        // A run that did not draw the watcher feature must get client two back
        // as a contender rather than as an idle observer.
        assert_eq!(Role::assign(2, 8, true, false), Role::Contender);
        assert_eq!(Role::assign(2, 8, true, true), Role::Watcher);
        assert_eq!(Role::assign(2, 8, false, false), Role::Contender);

        // The degradation guards are untouched by the gate: a Sleeper needs
        // three clients and a Watcher four, however the features fell.
        for client_count in 1..=2 {
            assert_eq!(
                Role::assign(1, client_count, true, true),
                Role::Contender,
                "a sleeper needs somebody to take over from it"
            );
        }
        assert_eq!(Role::assign(1, 3, true, true), Role::Sleeper);
        assert_eq!(Role::assign(1, 3, false, true), Role::Contender);
        assert_eq!(Role::assign(2, 3, true, true), Role::Contender);
        assert_eq!(Role::assign(2, 4, true, true), Role::Watcher);

        // Every other client contends whatever the features say.
        for client_id in [0, 3, 7] {
            for sleeper in [false, true] {
                for watcher in [false, true] {
                    assert_eq!(
                        Role::assign(client_id, 8, sleeper, watcher),
                        Role::Contender
                    );
                }
            }
        }
    }
}
