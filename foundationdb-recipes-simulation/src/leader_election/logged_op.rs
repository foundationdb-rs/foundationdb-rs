//! Running one primitive and its log record in a single transaction.
//!
//! Every operation the workload performs goes through [`Journal`]. It pairs the
//! recipe primitive with the [`log_schema`](super::log_schema) record that
//! describes it and commits both together, so the log holds exactly the
//! operations that happened, in the order FoundationDB ordered them.
//!
//! Two rules keep the log honest:
//!
//! - A semantic rejection (a denied claim, a lost renewal, a fenced write the
//!   register refused) is not an error. The transaction commits and the record
//!   says `Rejected`, because a failure path that never reaches the log is one
//!   the check phase cannot see. The previous suite lost every one of those.
//! - Infrastructure errors abort. They are the retry loop's business, and a
//!   half-applied operation must not leave a record claiming it happened.
//!
//! # Idempotency belongs here, not in the recipe
//!
//! The recipe sets no transaction options at all: the caller owns its
//! transactions. The simulation owns its own, and every logged transaction sets
//! `TransactionOption::AutomaticIdempotency`, because a versionstamped log
//! append is not naturally idempotent: retrying one after a
//! `commit_unknown_result` would leave two records for a single operation, and
//! replay would count that operation twice.
//!
//! That does not make the recipe's token-based recovery redundant, and
//! `UuidRecoveryNoDup` still has work to do: automatic idempotency does not
//! cover every hazard (multiversion clients, transactions outliving the
//! idempotency window), and those are exactly the cases where a retry has to
//! recognize its own record instead of claiming a second time.
//!
//! It does mean the recovery path is nearly unreachable by accident: the client
//! resolves the unknown commit itself, and the recipe's own recovery is never
//! asked anything. That is why the driver forces it, throwing away replies it
//! did receive and re-running the attempt later, and why
//! [`injected_unknown`](Journal::injected_unknown) exists to say so in the log.

use std::cell::Cell;
use std::sync::Arc;
use std::time::Duration;

use foundationdb::env::{Clock, Environment};
use foundationdb::options::{MutationType, TransactionOption};
use foundationdb::recipes::leader_election::{
    ClaimAttempt, ClaimOutcome, LeaderElection, LeaderRecord, LeaseDuration, LeaseGrant,
    LeaseObservation, RefreshAttempt, RefreshOutcome, ResignOutcome,
};
use foundationdb::recipes::ranked_register::{Rank, RankedRegister, WriteResult};
use foundationdb::tuple::{Subspace, pack, unpack};
use foundationdb::{FdbBindingError, FdbResult, RetryableTransaction};
use foundationdb_simulation::SimDatabase;
use futures::future::BoxFuture;

use super::clock::SkewedClock;
use super::log_schema::{LogRecord, ObservedIdentity, OpKind, Outcome, incomplete_log_key};

/// Wrap a recipe error so the retry loop can still find the `FdbError` inside
///
/// Boxing the error itself rather than its message is what keeps the
/// `source()` chain intact: a stringified `transaction_too_old` would become a
/// fatal error instead of a retried one.
fn custom<E>(error: E) -> FdbBindingError
where
    E: std::error::Error + Send + Sync + 'static,
{
    FdbBindingError::CustomError(Box::new(error))
}

fn nanos(duration: Duration) -> u64 {
    duration.as_nanos() as u64
}

/// What a claim transaction produced, plus the observation to thread onwards
pub(crate) type ClaimResult = (ClaimOutcome, LeaseObservation);

/// One client's instrumented view of the recipe
///
/// Owns the client's environment and clock, its operation counter and the two
/// recipe objects it drives. Every method here is one logged transaction.
pub(crate) struct Journal {
    /// The simulator's time and randomness, undistorted
    env: Environment,
    /// Shared rather than owned: the elector role hands the very same clock to
    /// the recipe's [`Environment`], so that the margin the recipe derives from
    /// its configured rate error covers the skew this client actually has.
    clock: Arc<SkewedClock>,
    election: LeaderElection,
    register: RankedRegister,
    log_subspace: Subspace,
    client_id: i32,
    leader_id: String,
    next_op_num: Cell<u64>,
}

impl Journal {
    /// Set a client up to drive and record the recipe
    ///
    /// `log_subspace` is where this journal's records land. The driver writes to
    /// [`log_subspace`](super::log_schema::log_subspace) and the elector role to
    /// [`elector_log_subspace`](super::log_schema::elector_log_subspace): two
    /// runs against two elections, judged separately.
    pub(crate) fn new(
        env: Environment,
        clock: Arc<SkewedClock>,
        election: LeaderElection,
        register: RankedRegister,
        log_subspace: Subspace,
        client_id: i32,
    ) -> Self {
        Self {
            env,
            clock,
            election,
            register,
            log_subspace,
            client_id,
            // The identifier replay reconstructs from a client id; the two must
            // agree or every record looks like it came from a stranger.
            leader_id: format!("process_{client_id}"),
            next_op_num: Cell::new(0),
        }
    }

    /// The identifier this client claims under
    pub(crate) fn leader_id(&self) -> &str {
        &self.leader_id
    }

    /// This client's clock
    pub(crate) fn clock(&self) -> &SkewedClock {
        &self.clock
    }

    /// This client's clock, shareable with another journal or an
    /// [`Environment`]
    pub(crate) fn clock_handle(&self) -> &Arc<SkewedClock> {
        &self.clock
    }

    /// The client this journal belongs to
    pub(crate) fn client_id(&self) -> i32 {
        self.client_id
    }

    /// The simulator's time and randomness, undistorted
    pub(crate) fn env(&self) -> &Environment {
        &self.env
    }

    /// True simulated time
    ///
    /// Only the check phase and the log's `sim_nanos` field may use this; the
    /// recipe is never handed anything but [`local_now`](Self::local_now).
    pub(crate) fn sim_now(&self) -> Duration {
        self.env.clock().monotonic()
    }

    /// This client's own reading of the current time
    ///
    /// The same clock the recipe would be handed: this client's skewed view of
    /// the one above.
    pub(crate) fn local_now(&self) -> Duration {
        self.clock.monotonic()
    }

    /// How many operations this client has logged
    pub(crate) fn ops_logged(&self) -> u64 {
        self.next_op_num.get()
    }

    // ========================================================================
    // THE WRAPPER
    // ========================================================================

    /// Run `op` and append the record it produced, in one transaction.
    ///
    /// `op` reads, decides and returns both its result and the record
    /// describing what it did; this method owns the parts that are the same for
    /// every operation: the idempotency option, the true-time stamp taken after
    /// the operation's reads, and the versionstamped append.
    async fn run<T, F, Fut>(&self, db: &SimDatabase, op: F) -> Result<T, FdbBindingError>
    where
        F: Fn(RetryableTransaction, bool) -> Fut,
        Fut: Future<Output = Result<(T, LogRecord), FdbBindingError>>,
    {
        let op_num = self.next_op_num.get();
        self.next_op_num.set(op_num + 1);

        db.run(|trx, maybe_committed| {
            let op = &op;
            let maybe_committed = bool::from(maybe_committed);
            async move {
                trx.set_option(TransactionOption::AutomaticIdempotency)?;
                let (value, mut record) = op(trx.clone(), maybe_committed).await?;
                record.maybe_committed |= maybe_committed;
                record.sim_nanos = nanos(self.sim_now());
                trx.atomic_op(
                    &incomplete_log_key(&self.log_subspace, self.client_id, op_num),
                    &record.encode(),
                    MutationType::SetVersionstampedKey,
                );
                Ok(value)
            }
        })
        .await
    }

    // ========================================================================
    // PRIMITIVES
    // ========================================================================

    /// Campaign for a term, installing its fence in the same transaction
    ///
    /// The record is read once before the primitive runs, so the log can say
    /// what the transaction decided against and whether the write it made was a
    /// first claim, a steal, or the adoption of a record an earlier execution
    /// of this same attempt had already committed.
    ///
    /// # Why the fence is installed here
    ///
    /// Winning a ballot fences nothing by itself: the register refuses an old
    /// rank only once a higher one has been *read* into it. Doing that in a
    /// transaction of its own leaves a window between the two commits in which
    /// the deposed leader's writes are still accepted, and a term that is won
    /// and then abandoned (a claim that outlived its own lease, say) leaves
    /// that window open for good.
    ///
    /// Installing the fence in the claim transaction closes it: the term change
    /// and the fence commit together, so a stale write either lands before the
    /// takeover, where it belongs to a term that still held, or after it, where
    /// the register refuses it. This is the strongest form of the activation
    /// step the composition contract requires, and it costs one extra key in a
    /// transaction that was already serialising on the leader record.
    pub(crate) async fn claim(
        &self,
        db: &SimDatabase,
        lease: LeaseDuration,
        attempt: &ClaimAttempt,
        attempt_id: u64,
        observation: LeaseObservation,
    ) -> Result<ClaimResult, FdbBindingError> {
        self.run(db, |trx, _| async move {
            let previous = self.election.leader(&trx).await.map_err(custom)?;
            let recovering = previous
                .as_ref()
                .is_some_and(|record| record.is_held_by(&self.leader_id, attempt.token()));

            // Sampled where the recipe asks for it: immediately after its own
            // read, so the observation window is measured against the read it
            // belongs to.
            let sampled: Cell<Option<Duration>> = Cell::new(None);
            let (outcome, updated) = self
                .election
                .try_claim(&trx, &self.leader_id, lease, attempt, observation, || {
                    let now = self.local_now();
                    sampled.set(Some(now));
                    now
                })
                .await
                .map_err(custom)?;
            let local = sampled.get().unwrap_or_else(|| self.local_now());

            let won = matches!(outcome, ClaimOutcome::Won(_));
            let mut record = LogRecord::new(match &previous {
                Some(record) if !record.is_vacant() => OpKind::Steal,
                _ => OpKind::Claim,
            });
            record.outcome = if won {
                Outcome::Applied
            } else {
                Outcome::Rejected
            };
            record.attempt_id = attempt_id;
            record.token = *attempt.token().as_bytes();
            record.leader_record_written = won && !recovering;
            record.recovery_noop = won && recovering;
            record.superseded = matches!(outcome, ClaimOutcome::Superseded);
            record.maybe_committed = attempt.maybe_committed();
            record.observed = previous.as_ref().map(identity_of);
            record.local_nanos = nanos(local);
            record.observation_start_nanos = updated.observed_since().map(nanos);
            if let ClaimOutcome::Won(grant) = &outcome {
                self.register
                    .read(&trx, grant.rank(0))
                    .await
                    .map_err(custom)?;
                record.ballot = grant.ballot();
                record.generation = grant.generation();
                record.lease_nanos = grant.lease().as_nanos();
            } else {
                record.lease_nanos = previous
                    .as_ref()
                    .and_then(LeaderRecord::lease)
                    .map_or(0, LeaseDuration::as_nanos);
            }

            Ok(((outcome, updated), record))
        })
        .await
    }

    /// Record that the driver threw away the reply to a claim that committed
    ///
    /// A transaction of its own, issued after the claim it describes has
    /// committed, so the marker exists if and only if that claim does. Putting
    /// it in the claim transaction would be the opposite of what it is for: a
    /// marker that vanished with the commit it marks could not tell the check
    /// phase that the injection ever happened.
    ///
    /// The claim itself is untouched. Everything the recovery needs is in the
    /// attempt, which the caller keeps and re-runs later.
    pub(crate) async fn injected_unknown(
        &self,
        db: &SimDatabase,
        attempt_id: u64,
        attempt: &ClaimAttempt,
        ballot: u64,
    ) -> Result<(), FdbBindingError> {
        self.run(db, |_, _| async move {
            let mut record = LogRecord::new(OpKind::InjectedUnknown);
            record.attempt_id = attempt_id;
            record.token = *attempt.token().as_bytes();
            record.ballot = ballot;
            record.maybe_committed = attempt.maybe_committed();
            record.local_nanos = nanos(self.local_now());
            Ok(((), record))
        })
        .await
    }

    /// Extend a term by one generation
    pub(crate) async fn refresh(
        &self,
        db: &SimDatabase,
        grant: &LeaseGrant,
        attempt: &RefreshAttempt,
    ) -> Result<RefreshOutcome, FdbBindingError> {
        self.run(db, |trx, _| async move {
            let previous = self.election.leader(&trx).await.map_err(custom)?;
            // The renewal our own earlier execution committed: the recipe
            // adopts it instead of writing a second time, and so must the log.
            let recovering = previous.as_ref().is_some_and(|record| {
                record.is_held_by(&self.leader_id, grant.token())
                    && record.generation() == attempt.expected_generation() + 1
            });
            let local = self.local_now();

            let outcome = self
                .election
                .refresh(&trx, grant, attempt)
                .await
                .map_err(custom)?;

            let applied = matches!(outcome, RefreshOutcome::Refreshed(_));
            let mut record = LogRecord::new(OpKind::Renew);
            record.outcome = if applied {
                Outcome::Applied
            } else {
                Outcome::Rejected
            };
            record.token = *grant.token().as_bytes();
            record.ballot = grant.ballot();
            record.generation = attempt.expected_generation() + 1;
            record.leader_record_written = applied && !recovering;
            record.recovery_noop = applied && recovering;
            record.observed = previous.as_ref().map(identity_of);
            record.local_nanos = nanos(local);
            record.lease_nanos = grant.lease().as_nanos();

            Ok((outcome, record))
        })
        .await
    }

    /// Hand the term back
    pub(crate) async fn resign(
        &self,
        db: &SimDatabase,
        grant: &LeaseGrant,
    ) -> Result<ResignOutcome, FdbBindingError> {
        self.run(db, |trx, _| async move {
            let previous = self.election.leader(&trx).await.map_err(custom)?;
            let recovering = previous
                .as_ref()
                .is_some_and(|record| record.is_vacant() && record.ballot() == grant.ballot());
            let local = self.local_now();

            let outcome = self.election.resign(&trx, grant).await.map_err(custom)?;

            let applied = matches!(outcome, ResignOutcome::Resigned);
            let mut record = LogRecord::new(OpKind::Resign);
            record.outcome = if applied {
                Outcome::Applied
            } else {
                Outcome::Rejected
            };
            record.token = *grant.token().as_bytes();
            // A resign preserves the identity it found, which is what lets the
            // successor take `ballot + 1` with no wait at all.
            record.ballot = previous
                .as_ref()
                .map_or(grant.ballot(), LeaderRecord::ballot);
            record.generation = previous
                .as_ref()
                .map_or(grant.generation(), LeaderRecord::generation);
            record.leader_record_written = applied && !recovering;
            record.recovery_noop = applied && recovering;
            record.observed = previous.as_ref().map(identity_of);
            record.local_nanos = nanos(local);

            Ok((outcome, record))
        })
        .await
    }

    // ========================================================================
    // FENCED WORK
    // ========================================================================

    /// Write to the ranked register under a leadership rank
    ///
    /// A plain ranked write: the fence was installed by the transaction that
    /// won the term, and re-reading here would only hide the thing this is for.
    /// A stale leader waking from a pause takes exactly this path, and its
    /// write must be refused by whatever fence has replaced its own.
    pub(crate) async fn fenced_write(
        &self,
        db: &SimDatabase,
        ballot: u64,
        rank: Rank,
        sequence: u32,
    ) -> Result<WriteResult, FdbBindingError> {
        self.run(db, |trx, _| async move {
            let local = self.local_now();
            let value = pack(&(self.client_id, ballot, sequence));
            let outcome = self
                .register
                .write(&trx, rank, &value)
                .await
                .map_err(custom)?;

            let mut record = LogRecord::new(OpKind::FencedWrite);
            record.outcome = if outcome.is_committed() {
                Outcome::Applied
            } else {
                Outcome::Rejected
            };
            record.ballot = ballot;
            record.generation = u64::from(sequence);
            record.local_nanos = nanos(local);

            Ok((outcome, record))
        })
        .await
    }

    /// Install the fence of a term: read the register at `rank`
    ///
    /// The activation step the fencing composition requires. The driver never
    /// calls it, because [`claim`](Self::claim) installs the fence in the claim
    /// transaction itself; the real [`LeaderElector`] does not, so a leader it
    /// elects owes this before any fenced work.
    ///
    /// Deliberately unlogged: a read of the register is not an operation of the
    /// election protocol, and what the check phase judges is which writes the
    /// fence went on to refuse rather than when it was installed.
    ///
    /// [`LeaderElector`]: foundationdb::recipes::leader_election::LeaderElector
    pub(crate) async fn install_fence(
        &self,
        db: &SimDatabase,
        rank: Rank,
    ) -> Result<(), FdbBindingError> {
        db.run(|trx, _| async move {
            self.register.read(&trx, rank).await.map_err(custom)?;
            Ok(())
        })
        .await
    }

    /// Who last wrote the ranked register, if anybody
    ///
    /// Deliberately unlogged. The Sleeper's barrier waits on this, but waiting
    /// is scaffolding for the scenario rather than a step of the protocol, and
    /// a log record of it would be a record of something the recipe never did.
    pub(crate) async fn register_writer(
        &self,
        db: &SimDatabase,
    ) -> Result<Option<(i32, u64, u32)>, FdbBindingError> {
        db.run(|trx, _| async move {
            match self.register.value(&trx).await.map_err(custom)? {
                Some(bytes) => Ok(Some(unpack(&bytes).map_err(custom)?)),
                None => Ok(None),
            }
        })
        .await
    }

    // ========================================================================
    // DISCOVERY
    // ========================================================================

    /// Read the leader record, optionally arming a watch on the term key
    ///
    /// The watch is created in the same transaction as the read it is anchored
    /// to and returned to be awaited after the commit, which is the only order
    /// that cannot miss a change between the two.
    pub(crate) async fn observe(
        &self,
        db: &SimDatabase,
        arm_watch: bool,
    ) -> Result<
        (
            Option<LeaderRecord>,
            Option<BoxFuture<'static, FdbResult<()>>>,
        ),
        FdbBindingError,
    > {
        self.run(db, |trx, _| async move {
            let current = self.election.leader(&trx).await.map_err(custom)?;
            let local = self.local_now();
            let watch = arm_watch.then(|| self.election.watch_term(&trx));

            let mut record = LogRecord::new(OpKind::Observe);
            record.observed = current.as_ref().map(identity_of);
            record.local_nanos = nanos(local);
            record.lease_nanos = current
                .as_ref()
                .and_then(LeaderRecord::lease)
                .map_or(0, LeaseDuration::as_nanos);

            Ok(((current, watch), record))
        })
        .await
    }

    // ========================================================================
    // BELIEF
    // ========================================================================

    /// Record that this client started (or extended) believing it leads
    ///
    /// Written after the claim or renewal that justifies it has committed, so
    /// the interval the check phase sees can only be wider than the one the
    /// client actually acted on.
    pub(crate) async fn belief_begin(
        &self,
        db: &SimDatabase,
        grant: &LeaseGrant,
        horizon: Duration,
    ) -> Result<(), FdbBindingError> {
        self.belief(db, OpKind::BeliefBegin, grant, horizon).await
    }

    /// Record that this client stopped believing it leads
    ///
    /// Written *before* the resign it precedes commits. The other order would
    /// leave a window in which the successor could already be believing while
    /// this client's belief was still open, and an orderly handover would look
    /// like an overlap.
    pub(crate) async fn belief_end(
        &self,
        db: &SimDatabase,
        grant: &LeaseGrant,
    ) -> Result<(), FdbBindingError> {
        self.belief(db, OpKind::BeliefEnd, grant, Duration::ZERO)
            .await
    }

    async fn belief(
        &self,
        db: &SimDatabase,
        op: OpKind,
        grant: &LeaseGrant,
        horizon: Duration,
    ) -> Result<(), FdbBindingError> {
        self.belief_record(
            db,
            op,
            grant.ballot(),
            *grant.token().as_bytes(),
            horizon,
            grant.lease().as_nanos(),
        )
        .await
    }

    /// Record a belief without holding the grant that justifies it
    ///
    /// The elector role has a [`LeaseHandle`] rather than a [`LeaseGrant`]: the
    /// recipe keeps the grant to itself and hands out the ballot, the horizon
    /// and nothing else. So the token is written all-zero, which costs nothing,
    /// because replay pairs a begin with its end by `(client_id, ballot)` and
    /// reads the token of a belief record for no purpose at all.
    ///
    /// [`LeaseHandle`]: foundationdb::recipes::leader_election::LeaseHandle
    pub(crate) async fn belief_begin_at(
        &self,
        db: &SimDatabase,
        ballot: u64,
        horizon: Duration,
        lease: LeaseDuration,
    ) -> Result<(), FdbBindingError> {
        self.belief_record(
            db,
            OpKind::BeliefBegin,
            ballot,
            [0u8; 16],
            horizon,
            lease.as_nanos(),
        )
        .await
    }

    /// Record the end of a belief without holding the grant
    ///
    /// The grant-free counterpart of [`belief_end`](Self::belief_end), with the
    /// same ordering rule: it is written before whatever hands the term back.
    pub(crate) async fn belief_end_at(
        &self,
        db: &SimDatabase,
        ballot: u64,
    ) -> Result<(), FdbBindingError> {
        self.belief_record(db, OpKind::BeliefEnd, ballot, [0u8; 16], Duration::ZERO, 0)
            .await
    }

    async fn belief_record(
        &self,
        db: &SimDatabase,
        op: OpKind,
        ballot: u64,
        token: [u8; 16],
        horizon: Duration,
        lease_nanos: u64,
    ) -> Result<(), FdbBindingError> {
        self.run(db, |_, _| async move {
            let mut record = LogRecord::new(op);
            record.ballot = ballot;
            record.token = token;
            record.local_nanos = nanos(self.local_now());
            record.horizon_nanos = nanos(horizon);
            record.lease_nanos = lease_nanos;
            Ok(((), record))
        })
        .await
    }
}

/// The pair observers track, as the log spells it
fn identity_of(record: &LeaderRecord) -> ObservedIdentity {
    ObservedIdentity {
        ballot: record.ballot(),
        generation: record.generation(),
        vacant: record.is_vacant(),
    }
}
