//! The recipe's own elector, run as a role.
//!
//! Every other role in [`roles`](super::roles) emulates the recipe's async
//! handle layer on simulated time: campaign, renew, hard-stop at the belief
//! horizon, resign. This one runs the layer itself. [`LeaderElector`] is
//! constructed with this client's skewed clock, the simulator's generator and a
//! [`SimTimer`](super::timer::SimTimer), and then asked to lead; the loop that
//! decides when to stop believing is the recipe's, not ours.
//!
//! # A separate election, judged separately
//!
//! The elector campaigns in its own subspace, fences with its own ranked
//! register and writes to its own log. Nothing it does is journalled at the
//! transaction level, and that is the design rather than an omission: the
//! recipe owns those transactions, we do not get to wrap them, and wrapping
//! them would mean testing a copy of the elector instead of the elector. What
//! the log holds is what only this side knows, which is when a client *believed*
//! it led and what it wrote under that belief;
//! [`elector_invariants`](super::elector_invariants) judges the run by pairing
//! that against the recipe's own history subspace, in commit order. Safety by
//! effect: the question is never "did the elector take the right code path", it
//! is "did a write land outside the term that authorized it".
//!
//! # What the work closure owes
//!
//! [`LeaderElector::lead`] hands the work a [`LeaseHandle`] and drops the work
//! future the moment the term ends, so the closure here is written to be
//! droppable at any await point:
//!
//! 1. the belief is logged before anything is done under it, and re-logged
//!    whenever a renewal moves the horizon out;
//! 2. the activation fence is installed as the first action of every term. The
//!    driver gets this for free (its claim transaction installs the fence), the
//!    real elector does not: winning a ballot fences nothing by itself;
//! 3. every fenced write races the time left before the horizon, and is dropped
//!    rather than retried past it;
//! 4. a belief-end is written on the way out, whichever way out it took, and
//!    only when the term is still live and the client still strictly inside the
//!    horizon it logged. Past it there is nothing to say: the horizon already
//!    ended the belief, and a record written afterwards would claim the client
//!    believed longer than it was entitled to.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use foundationdb::env::{Clock, Environment};
use foundationdb::recipes::leader_election::{
    ElectorConfig, LeadOutcome, LeaderElection, LeaderElector, LeaseDuration, LeaseHandle,
    Result as ElectorResult,
};
use foundationdb::recipes::ranked_register::RankedRegister;
use foundationdb::tuple::Subspace;
use foundationdb_simulation::{Severity, SimDatabase, WorkloadContext, details};
use futures::future::Either;

use super::clock::{SkewMode, SkewedClock};
use super::liveness::{LivenessGuard, next_tick};
use super::log_schema::elector_log_subspace;
use super::logged_op::Journal;
use super::timer::SimTimer;

/// The election the elector role campaigns in
///
/// Its own, and deliberately not the driver's: the two runs share a database,
/// not a history, and mixing them would make either side's replay a fiction.
pub(crate) fn election() -> LeaderElection {
    LeaderElection::new(Subspace::all().subspace(&("le_elector",)))
}

/// The ranked register the elector role fences its work with
pub(crate) fn register() -> RankedRegister {
    RankedRegister::new(Subspace::all().subspace(&("le_elector_register",)))
}

// ============================================================================
// PURE RULES
// ============================================================================

/// Whether a horizon reading is worth another belief-begin
///
/// Replay merges the begins of one `(client, ballot)` by taking the largest
/// horizon, so a begin that does not move the horizon out says nothing that is
/// not already in the log.
pub(crate) fn should_log_extension(logged: Option<Duration>, horizon: Duration) -> bool {
    logged.is_none_or(|logged| horizon > logged)
}

/// Whether a belief may still be closed with a record
///
/// Strictly inside the horizon. At the horizon the belief is already over, and
/// a successor may already have started counting, so a record written then
/// would be a claim to have believed longer than the term allowed.
pub(crate) fn may_log_end(now: Duration, horizon: Duration) -> bool {
    now < horizon
}

// ============================================================================
// SETUP AND COUNTERS
// ============================================================================

/// Everything the elector role borrows from the client hosting it
pub(crate) struct ElectorSetup<'a> {
    /// The simulator handle: delays and traces
    pub(crate) context: &'a WorkloadContext,
    /// The simulator's undistorted time and randomness
    pub(crate) env: &'a Environment,
    /// This client's skewed view of time
    ///
    /// Shared with the recipe rather than copied: the margin the recipe derives
    /// from [`ElectorConfig::max_clock_rate_error`] has to cover the skew this
    /// client actually has, and it can only do that if the two are the same
    /// clock.
    pub(crate) clock: &'a Arc<SkewedClock>,
    /// The client running this elector
    pub(crate) client_id: i32,
    /// The lease the plan drew
    pub(crate) lease: LeaseDuration,
    /// What the clocks are allowed to do
    pub(crate) skew_mode: SkewMode,
    /// How long a client waits between actions
    pub(crate) step: Duration,
    /// True simulated time at which the run ends
    pub(crate) deadline: Duration,
    /// How many operations this role's journal may log
    ///
    /// Its own budget, not a share of the hosting client's: the elector writes
    /// to its own log subspace and is judged on its own.
    pub(crate) op_ceiling: u64,
}

/// What one client's elector achieved
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct ElectorCounters {
    /// Terms won, counted where the work closure starts
    pub(crate) acquisitions: u64,
    /// Ranked-register writes that committed
    pub(crate) fenced_applied: u64,
    /// Ranked-register writes the fence refused
    pub(crate) fenced_rejected: u64,
    /// Terms the elector reported as lost
    pub(crate) lease_losses: u64,
    /// Terms handed back cleanly
    pub(crate) resigns: u64,
    /// Operations that failed with something other than a protocol refusal
    pub(crate) errors: u64,
}

/// What the work closure has told the log so far
///
/// Shared between the closure and the loop around it through an [`Rc`]: the
/// work future has no `Send` bound, and the loop needs to know what the closure
/// managed to write before it was dropped.
#[derive(Debug, Default)]
struct BeliefBookkeeping {
    /// The term the closure is serving
    ballot: u64,
    /// The furthest horizon a belief-begin was logged for, on this client's own
    /// clock
    logged_horizon: Option<Duration>,
    /// Whether the closure closed the belief itself
    ended: bool,
}

// ============================================================================
// THE ROLE
// ============================================================================

/// One client running the recipe's elector
struct ElectorRole {
    context: WorkloadContext,
    journal: Journal,
    lease: LeaseDuration,
    step: Duration,
    deadline: Duration,
    counters: Cell<ElectorCounters>,
}

/// Play the elector role until the run's simulated deadline
///
/// Errors are traced and end this client's participation rather than failing
/// the run: what a client failed to do shows up in the check phase as missing
/// progress, which is a judgement the whole log gets to make.
pub(crate) async fn run(setup: &ElectorSetup<'_>, db: &SimDatabase) -> ElectorCounters {
    let role = ElectorRole {
        context: setup.context.clone(),
        journal: Journal::new(
            setup.env.clone(),
            Arc::clone(setup.clock),
            election(),
            register(),
            elector_log_subspace(),
            setup.client_id,
            setup.op_ceiling,
        ),
        lease: setup.lease,
        step: setup.step,
        deadline: setup.deadline,
        counters: Cell::new(ElectorCounters::default()),
    };

    let config = match elector_config(setup.lease, setup.skew_mode) {
        Ok(config) => config,
        Err(error) => {
            role.failed("LeaderElectionElectorConfigInvalid", &error);
            return role.counters.get();
        }
    };

    // The environment the recipe reads: this client's skewed clock, so the
    // safety margin covers the skew that was injected, and the simulator's own
    // generator, so the campaign jitter and every claim token are part of the
    // run's reproducible state rather than ambient randomness.
    let env = Environment::new(
        Arc::clone(setup.clock) as Arc<dyn Clock>,
        Arc::clone(setup.env.rng()),
    );

    let elector = match LeaderElector::new(
        Arc::clone(db),
        election().subspace().clone(),
        role.journal.leader_id(),
        config,
        env,
        Arc::new(SimTimer::new(setup.context)),
    ) {
        Ok(elector) => elector,
        Err(error) => {
            role.failed("LeaderElectionElectorUnbuildable", &error);
            return role.counters.get();
        }
    };

    role.lead_until_deadline(db, &elector).await;
    // The elector, and the database reference inside it, are dropped with this
    // frame: `check_database_ref` fails the run if any survives the phase.
    role.counters.get()
}

/// The lease schedule the elector runs on
///
/// The plan's lease, and the rate error the plan's skew mode actually injects,
/// so that the margin the recipe subtracts from every horizon is derived from
/// the same number the check phase builds its tolerances from. The scheduling
/// allowance keeps the recipe's default: the simulator's timers are exact, but
/// a commit still takes simulated time the client's own timestamps do not see.
fn elector_config(lease: LeaseDuration, skew_mode: SkewMode) -> ElectorResult<ElectorConfig> {
    ElectorConfig::new(lease.as_duration())?.with_max_clock_rate_error(skew_mode.max_rate_error())
}

impl ElectorRole {
    /// Win terms, serve them, and campaign again until the run ends
    async fn lead_until_deadline(&self, db: &SimDatabase, elector: &LeaderElector) {
        let mut guard = LivenessGuard::new();
        while self.journal.sim_now() < self.deadline {
            if !guard.tick(self.journal.sim_now()) {
                self.stalled("elector terms");
                return;
            }
            let remaining = self.deadline.saturating_sub(self.journal.sim_now());
            let book = Rc::new(RefCell::new(BeliefBookkeeping::default()));

            let outcome = {
                let attempt = elector.lead(|handle| {
                    let book = Rc::clone(&book);
                    async move { self.serve(db, handle, &book).await }
                });
                futures::pin_mut!(attempt);
                let give_up = Box::pin(self.context.delay(remaining));

                match futures::future::select(attempt, give_up).await {
                    Either::Left((outcome, _)) => outcome,
                    // The campaign has no timeout of its own: an elector that
                    // never wins waits forever, and dropping the future is the
                    // documented way to give up.
                    Either::Right((result, _)) => {
                        self.gave_up(result.is_err());
                        return;
                    }
                }
            };

            match outcome {
                // The closure ran to its own end, so it has already closed the
                // belief if it was still entitled to.
                Ok(LeadOutcome::Completed { released, .. }) => {
                    if released {
                        self.bump(|counters| counters.resigns += 1);
                    }
                }
                Ok(LeadOutcome::LeaseLost) => {
                    self.bump(|counters| counters.lease_losses += 1);
                    self.close_lost_belief(db, &book).await;
                }
                Err(error) => {
                    self.failed("LeaderElectionElectorFailed", &error);
                    return;
                }
            }
        }
    }

    /// Serve one term, from the inside of [`LeaderElector::lead`]
    ///
    /// Every way out of [`work`](Self::work) leads here, including the ones that
    /// gave up on an error, because returning is what makes `lead` hand the term
    /// back: a belief left open while the recipe resigns would still be open
    /// when the successor starts believing. The rule that decides whether an end
    /// may be written is the same one either way, and it refuses exactly when
    /// the term is already over, which is the case where the horizon has spoken
    /// and there is nothing left to say.
    async fn serve(
        &self,
        db: &SimDatabase,
        handle: LeaseHandle,
        book: &RefCell<BeliefBookkeeping>,
    ) {
        let ballot = handle.ballot();
        book.borrow_mut().ballot = ballot;
        self.bump(|counters| counters.acquisitions += 1);

        self.work(db, &handle, book).await;

        if handle.check().is_ok() && may_log_end(self.journal.local_now(), handle.believed_until())
        {
            self.end_belief(db, ballot, book).await;
        }
    }

    /// Everything done under one term
    async fn work(
        &self,
        db: &SimDatabase,
        handle: &LeaseHandle,
        book: &RefCell<BeliefBookkeeping>,
    ) {
        // (1) The belief, before anything is done under it.
        if !self.begin_belief(db, handle, book).await {
            return;
        }

        // (2) The activation fence, first action of the term. Winning a ballot
        // fences nothing by itself, and unlike the driver's claim transaction
        // the recipe does not install one.
        let rank = match handle.next_rank() {
            Ok(rank) => rank,
            Err(_) => return,
        };
        if let Err(error) = self.journal.install_fence(db, rank).await {
            self.failed("LeaderElectionElectorFenceFailed", &error);
            return;
        }

        // (3) Work, until the run ends or the term does.
        let mut guard = LivenessGuard::new();
        // Paced on an absolute cursor rather than a step per round: the cursor
        // moves before the wait, so a wait that returned instantly still leaves
        // the next round asking for a real one. See `Driver::pace_from`.
        let mut next = self.journal.local_now();
        while self.journal.sim_now() < self.deadline {
            if !guard.tick(self.journal.sim_now()) {
                self.stalled("elector work");
                return;
            }
            // The term is gone: either the horizon passed, which already ended
            // the belief, or a successor took over having waited out a whole
            // lease. Either way there is no end left to write, which is what the
            // rule in `serve` decides.
            if handle.check().is_err() {
                return;
            }
            if !self.begin_belief(db, handle, book).await {
                return;
            }
            if !self.fenced_step(db, handle).await {
                return;
            }
            let (cursor, wait) = next_tick(next, self.step, self.journal.local_now());
            next = cursor;
            if !wait.is_zero() && !self.delay(wait).await {
                return;
            }
        }
    }

    /// Log a belief-begin if the horizon has moved out since the last one
    ///
    /// Returns whether the caller may carry on: a log this client cannot write
    /// to would make everything it did afterwards invisible to the check phase,
    /// so it stops instead.
    async fn begin_belief(
        &self,
        db: &SimDatabase,
        handle: &LeaseHandle,
        book: &RefCell<BeliefBookkeeping>,
    ) -> bool {
        let horizon = handle.believed_until();
        let logged = book.borrow().logged_horizon;
        if !should_log_extension(logged, horizon) {
            return true;
        }
        match self
            .journal
            .belief_begin_at(db, handle.ballot(), horizon, self.lease)
            .await
        {
            Ok(()) => {
                book.borrow_mut().logged_horizon = Some(horizon);
                true
            }
            Err(error) => {
                self.failed("LeaderElectionElectorBeliefFailed", &error);
                false
            }
        }
    }

    /// Close the belief this client opened
    async fn end_belief(&self, db: &SimDatabase, ballot: u64, book: &RefCell<BeliefBookkeeping>) {
        match self.journal.belief_end_at(db, ballot).await {
            Ok(()) => book.borrow_mut().ended = true,
            Err(error) => self.failed("LeaderElectionElectorBeliefFailed", &error),
        }
    }

    /// Close a belief whose work closure was dropped mid-term
    ///
    /// Only while this client's own clock is still inside the horizon it last
    /// logged. Past it the horizon has already spoken, and a belief-end written
    /// afterwards would widen the interval the check phase sees beyond what the
    /// client was entitled to believe.
    async fn close_lost_belief(&self, db: &SimDatabase, book: &RefCell<BeliefBookkeeping>) {
        let (ballot, logged, ended) = {
            let book = book.borrow();
            (book.ballot, book.logged_horizon, book.ended)
        };
        let horizon = match logged {
            Some(horizon) if !ended => horizon,
            _ => return,
        };
        if may_log_end(self.journal.local_now(), horizon) {
            self.end_belief(db, ballot, book).await;
        }
    }

    /// One piece of fenced work, given up at the horizon
    ///
    /// The horizon is a hard stop for work and not only for renewals: a write
    /// that keeps retrying past it is a leader acting on a term it has stopped
    /// believing in. The same race the driver runs, with the handle in place of
    /// the driver's own bookkeeping.
    ///
    /// Returns whether the caller may carry on.
    async fn fenced_step(&self, db: &SimDatabase, handle: &LeaseHandle) -> bool {
        let remaining = handle
            .believed_until()
            .saturating_sub(self.journal.local_now());
        if remaining.is_zero() {
            return false;
        }
        let rank = match handle.next_rank() {
            Ok(rank) => rank,
            Err(_) => return false,
        };

        // Scoped so the pinned transaction is dropped before the counters are
        // touched: it borrows the journal for as long as it lives.
        let outcome = {
            // The ballot occupies the high half of a rank and the fencing
            // sequence the low half, so what the log wants as a sequence is the
            // rank's process id.
            let work = self
                .journal
                .fenced_write(db, handle.ballot(), rank, rank.process_id());
            futures::pin_mut!(work);
            let horizon = Box::pin(self.context.delay(remaining));

            match futures::future::select(work, horizon).await {
                Either::Left((outcome, _)) => Some(outcome),
                // Dropped with this frame. It may still land, which is exactly
                // why the fence, and not the horizon, is what makes it safe.
                Either::Right((Ok(()), _)) => None,
                // A failed delay is not a horizon that arrived: the simulator is
                // tearing this client down, so the role stops rather than
                // treating an unpaced loop as work given up on time.
                Either::Right((Err(error), _)) => {
                    self.failed("LeaderElectionElectorDelayFailed", &error);
                    return false;
                }
            }
        };

        match outcome {
            Some(Ok(result)) => {
                self.bump(|counters| {
                    if result.is_committed() {
                        counters.fenced_applied += 1;
                    } else {
                        counters.fenced_rejected += 1;
                    }
                });
                true
            }
            Some(Err(error)) => {
                self.failed("LeaderElectionElectorWriteFailed", &error);
                false
            }
            // Abandoned at the horizon: the next `check` is what notices.
            None => true,
        }
    }

    // ========================================================================
    // SHARED
    // ========================================================================

    fn bump(&self, apply: impl FnOnce(&mut ElectorCounters)) {
        let mut counters = self.counters.get();
        apply(&mut counters);
        self.counters.set(counters);
    }

    /// Report something that ended this client's participation
    fn failed<E: std::fmt::Debug>(&self, name: &str, error: &E) {
        self.bump(|counters| counters.errors += 1);
        self.trace(
            Severity::WarnAlways,
            name,
            details![
                "Client" => self.journal.client_id(),
                "Error" => format!("{error:?}")
            ],
        );
    }

    /// Report a loop that went round without simulated time moving
    ///
    /// The backstop for a wait that stopped waiting. Loud, and terminal for
    /// this role: everything the client did after the clock stood still was
    /// unpaced, and the check phase judges what the log holds either way.
    fn stalled(&self, loop_name: &str) {
        self.bump(|counters| counters.errors += 1);
        self.trace(
            Severity::WarnAlways,
            "LeaderElectionElectorStalled",
            details![
                "Client" => self.journal.client_id(),
                "Loop" => loop_name,
                "Iterations" => LivenessGuard::STALL_LIMIT,
                "SimNanos" => self.journal.sim_now().as_nanos() as u64
            ],
        );
    }

    /// Report the role ending because its give-up delay resolved
    ///
    /// Ordinary at the deadline: an elector that never won waits forever, and
    /// dropping the future is how it gives up. Any other way of getting here is
    /// the delay having failed rather than fired, either reported directly by
    /// `delay_failed` or inferred from the clock not having reached the
    /// deadline, and that is the same class of defect
    /// [`stalled`](Self::stalled) exists for and gets the same volume.
    ///
    /// This is also the only thing that notices a `SimTimer` sleep parking
    /// forever: the recipe's own loops have no way to report a timer that never
    /// completes, so the race outside them is where it surfaces.
    fn gave_up(&self, delay_failed: bool) {
        let now = self.journal.sim_now();
        if delay_failed || now < self.deadline {
            self.bump(|counters| counters.errors += 1);
            self.trace(
                Severity::WarnAlways,
                "LeaderElectionElectorGaveUpEarly",
                details![
                    "Client" => self.journal.client_id(),
                    "DelayFailed" => delay_failed,
                    "SimNanos" => now.as_nanos() as u64,
                    "DeadlineNanos" => self.deadline.as_nanos() as u64
                ],
            );
            return;
        }
        self.trace(
            Severity::Info,
            "LeaderElectionElectorGaveUp",
            details!["Client" => self.journal.client_id()],
        );
    }

    /// Wait, and say whether the wait happened
    ///
    /// `false` means the delay failed, which is not a wait that finished early:
    /// it is this client being torn down, and the caller ends the role rather
    /// than going round an unpaced loop.
    async fn delay(&self, duration: Duration) -> bool {
        match self.context.delay(duration).await {
            Ok(()) => true,
            Err(error) => {
                self.failed("LeaderElectionElectorDelayFailed", &error);
                false
            }
        }
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

    const SEC: Duration = Duration::from_secs(1);

    #[test]
    fn a_belief_is_only_relogged_when_the_horizon_moves_out() {
        // The first one always: nothing has been logged, and the interval has
        // to exist before anything is done under it.
        assert!(should_log_extension(None, SEC));

        // A renewal that pushed the horizon out is worth a record; a poll that
        // found the same horizon is not, because replay merges begins by taking
        // the largest horizon and would learn nothing.
        assert!(should_log_extension(Some(SEC), 2 * SEC));
        assert!(!should_log_extension(Some(2 * SEC), 2 * SEC));
        assert!(!should_log_extension(Some(2 * SEC), SEC));
    }

    #[test]
    fn a_belief_end_stops_being_writable_at_the_horizon() {
        assert!(may_log_end(SEC, 2 * SEC));
        // The boundary is exclusive: at the horizon the belief is already over
        // and a successor may have started counting.
        assert!(!may_log_end(2 * SEC, 2 * SEC));
        assert!(!may_log_end(3 * SEC, 2 * SEC));
    }

    #[test]
    fn the_config_admits_the_skew_the_plan_injects() {
        // Every lease the plan can draw, against every skew mode: the recipe
        // validates that the renewal schedule fits inside the lease once the
        // margin is taken out, and a plan that cannot be honoured would leave
        // the run with no elector at all.
        for lease_secs in [1.0f64, 2.0, 3.0, 4.0, 8.0, 16.0] {
            let lease = LeaseDuration::new(Duration::from_secs_f64(lease_secs)).unwrap();
            for mode in [SkewMode::None, SkewMode::Random, SkewMode::Extreme] {
                let config = elector_config(lease, mode)
                    .unwrap_or_else(|error| panic!("lease {lease_secs}s, {mode:?}: {error}"));
                assert_eq!(config.max_clock_rate_error(), mode.max_rate_error());
                assert!(config.renew_interval() + config.safety_margin() < lease.as_duration());
            }
        }
    }

    #[test]
    fn the_two_elections_never_share_a_key() {
        // One database, two runs: a prefix collision would make either side's
        // replay a fiction, and the check phase would have no way to tell.
        let driver = Subspace::all().subspace(&("leader_election",));
        let (begin, end) = election().subspace().range();
        let (driver_begin, driver_end) = driver.range();
        assert!(end <= driver_begin || driver_end <= begin);

        let (register_begin, _) = register().subspace().range();
        assert!(!register_begin.starts_with(&begin));
    }
}
