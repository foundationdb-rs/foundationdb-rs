// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! # Leader Election for FoundationDB
//!
//! One contested record decides who leads. A term is identified by a `ballot`
//! that never resets, and a contender may only take a term from a live holder
//! after watching the record hold still, on its own monotonic clock, for at
//! least the lease that record advertises. No wall clock is ever compared
//! across processes, and a resigned term is reclaimed immediately, so an
//! orderly handover costs nothing while a crash costs one lease.
//!
//! ```no_run
//! use foundationdb::Database;
//! use foundationdb::env::Environment;
//! use foundationdb::recipes::leader_election::{
//!     ElectorConfig, LeadOutcome, LeaderElectionError, LeaderElector, LeaseLostError, Timer,
//! };
//! use foundationdb::tuple::Subspace;
//! use futures::future::BoxFuture;
//! use std::sync::Arc;
//! use std::time::Duration;
//!
//! // The handle layer reads time through the `Clock` of its `Environment` and
//! // waits through the `Timer` trait, so it is not tied to any runtime. This
//! // is what the `recipes-leader-election-tokio` feature ships as
//! // `TokioClock` and `TokioTimer`.
//! #[derive(Debug)]
//! struct TokioTimer;
//!
//! impl Timer for TokioTimer {
//!     fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()> {
//!         Box::pin(tokio::time::sleep(duration))
//!     }
//! }
//!
//! async fn serve(db: Arc<Database>) -> Result<(), LeaderElectionError> {
//!     let elector = LeaderElector::new(
//!         db,
//!         Subspace::all().subspace(&"my-service/election"),
//!         "worker-7",
//!         ElectorConfig::new(Duration::from_secs(10))?,
//!         // The machine clock and a generator seeded from entropy. A seeded
//!         // environment instead makes the whole campaign replay.
//!         Environment::default(),
//!         Arc::new(TokioTimer),
//!     )?;
//!
//!     let outcome = elector
//!         .lead(|handle| async move {
//!             let mut processed = 0u64;
//!             for _ in 0..5 {
//!                 // Worth doing before every unit of work: the handle goes
//!                 // stale on its own clock, with nobody having to tell it.
//!                 handle.check()?;
//!                 processed += 1;
//!             }
//!             Ok::<_, LeaseLostError>(processed)
//!         })
//!         .await?;
//!
//!     match outcome {
//!         LeadOutcome::Completed { value, released } => {
//!             println!("work ended with {value:?}, term handed back: {released}");
//!         }
//!         LeadOutcome::LeaseLost => println!("the term ended before the work did"),
//!     }
//!     Ok(())
//! }
//! ```
//!
//! # Consider the alternatives first
//!
//! Electing a leader buys serialization at the cost of a failure mode: there
//! are windows, bounded but real, in which two processes believe they lead.
//! Everything below is about making those windows harmless, and none of it is
//! free. Before reaching for an election, check whether the problem dissolves:
//!
//! - **Idempotent operations.** If applying the same work twice is
//!   indistinguishable from applying it once, two leaders are a performance
//!   problem, not a correctness one.
//! - **Optimistic concurrency.** FoundationDB transactions are serializable.
//!   Work that can be expressed as a compare-and-set on the state it touches
//!   needs no leader at all, only a retry loop.
//! - **Tolerating two leaders.** Duplicated effort is often cheaper than the
//!   operational weight of an election, especially for work that is
//!   short, cheap, and self-correcting.
//!
//! Reach for an election when the work is expensive to duplicate, has effects
//! that are not idempotent, or must be serialized across systems that do not
//! share the transaction.
//!
//! # Two API layers
//!
//! [`LeaderElection`] is the protocol, one step per transaction: read the
//! record, [`try_claim`](LeaderElection::try_claim),
//! [`refresh`](LeaderElection::refresh), [`resign`](LeaderElection::resign),
//! [`watch_term`](LeaderElection::watch_term). Every step is a pure function of
//! the record it read, the caller's observation state and a caller-supplied
//! instant, so `db.run` may re-execute it and a deterministic simulator may
//! drive it. Callers at this layer own their own timing, including the decision
//! about when to stop believing they lead.
//!
//! [`LeaderElector`] is the loop around it: campaign, hold the term while the
//! caller's work runs, renew in the same task, stop believing strictly before
//! any contender could take over, hand the term back at the end. It is the
//! layer most applications want.
//!
//! ```no_run
//! use foundationdb::Database;
//! use foundationdb::recipes::leader_election::{
//!     ClaimAttempt, ClaimOutcome, ClaimToken, LeaderElection, LeaderElectionError, LeaseDuration,
//!     LeaseObservation,
//! };
//! use std::sync::Mutex;
//! use std::time::Duration;
//!
//! /// One campaign round at the primitive layer.
//! async fn try_once(
//!     db: &Database,
//!     election: &LeaderElection,
//!     // Both of these outlive the transaction on purpose: how long this
//!     // process has watched the record hold still is the only thing that ever
//!     // authorizes a steal.
//!     observation: &Mutex<LeaseObservation>,
//!     now: &(dyn Fn() -> Duration + Sync),
//! ) -> Result<ClaimOutcome, LeaderElectionError> {
//!     let lease = LeaseDuration::new(Duration::from_secs(10))?;
//!     // Created before the transaction: it anchors the lease before the write
//!     // is issued, and lets a retry recognize a claim of its own.
//!     let attempt = ClaimAttempt::new(ClaimToken::generate(), now())?;
//!     let attempt = &attempt;
//!
//!     db.run(|txn, _| async move {
//!         let seen = *observation.lock().unwrap();
//!         let (outcome, updated) = election
//!             .try_claim(&txn, "worker-7", lease, attempt, seen, || now())
//!             .await?;
//!         *observation.lock().unwrap() = updated;
//!         Ok(outcome)
//!     })
//!     .await
//! }
//! ```
//!
//! # Three levels of exclusion
//!
//! "Mutual exclusion" is not one property here, and saying it without
//! qualification is how leases get misused. This recipe provides three
//! different things, with three different strengths.
//!
//! **Record-level exclusion is unconditional.** At most one process holds a
//! given ballot, always. Every transition is a compare-and-set on one key under
//! FoundationDB's serializable isolation, so two claimants of the same term is
//! not a rare case, it is not a case at all. This costs no assumptions
//! whatsoever.
//!
//! **Fencing is unconditional once activated.** A term's ballot orders every
//! operation that term performs above every operation of every earlier term.
//! Composed with the `RankedRegister` recipe (see below), a dispossessed
//! leader's writes are rejected by the database itself, no matter what that
//! leader believes. The activation step is mandatory and is described in the
//! fencing section.
//!
//! **Belief-level exclusion is best effort.** "Only one process at a time
//! *thinks* it leads" cannot be established by any protocol without a clock
//! assumption, and this one states its assumption rather than hiding it: every
//! participant's clock runs within
//! [`max_clock_rate_error`](ElectorConfig::max_clock_rate_error) of real time
//! (0.1% by default). From that bound, [`ElectorConfig`] derives a safety
//! margin and the holder hard-stops at `acquired_at + lease - margin`, which is
//! strictly before the earliest instant any contender's observation window can
//! close. See [`ElectorConfig::safety_margin`] for the derivation. If a process
//! is suspended for a minute by its hypervisor, or its clock is stepped by an
//! operator, that assumption fails and two processes may briefly believe they
//! lead. Fencing is what makes that survivable, which is why work with real
//! consequences should be fenced rather than merely gated on a status check.
//!
//! # Stealing without clocks
//!
//! Nothing in this protocol compares one process's clock with another's.
//! Instead a contender times the record: the stored identity is
//! `(ballot, generation)`, every applied write changes it (claims and steals
//! bump the ballot, renewals bump the generation), and a steal is authorized
//! only after the same identity has been observed continuously for the lease
//! that record advertises, measured on the observer's own monotonic clock. That
//! is the amazon-dynamodb-lock-client pattern, and it means clock *offsets* are
//! irrelevant. Only rate error matters, and only through the safety margin.
//!
//! A [`LeaseObservation`] carries that window between calls, which is why it
//! must be threaded through successive
//! [`try_claim`](LeaderElection::try_claim) calls and never shared between
//! processes. A fresh observation has seen nothing, so the first call after one
//! can never steal, however long the record has actually been sitting there.
//!
//! Advertised leases are clamped to
//! [`max_advertised_lease`](LeaderElection::max_advertised_lease) before being
//! waited on, so one misconfigured claimant cannot sterilize an election for an
//! unbounded time. Every participant must be configured with the same ceiling:
//! a contender with a lower one will steal from a leader that still believes it
//! leads.
//!
//! # Resigning is not crashing
//!
//! [`resign`](LeaderElection::resign) writes a *vacant* record that preserves
//! the ballot rather than clearing the key. The successor sees "the holder said
//! it was done" and takes `ballot + 1` with no observation wait at all, while a
//! crash costs the successor a full lease. That asymmetry is deliberate, and it
//! is Chubby's: an orderly release is information no timeout can produce, so it
//! is worth its own encoding.
//!
//! The precondition is caller quiescence. Resign after the work that the term
//! authorized has stopped, never before, and note that
//! [`ResignOutcome::NotHolder`] may also mean "an earlier resign of this term
//! committed and the reply was lost". Either way the caller stays stopped.
//!
//! # Recovering a claim whose reply was lost
//!
//! A commit that returns `commit_unknown_result` may or may not have landed,
//! and no read can tell the two apart afterwards. A claim therefore carries a
//! [`ClaimToken`] generated by the client and embedded in the record, so a
//! retry can recognize the record its own earlier execution wrote (the ZooKeeper
//! GUID trick). Recovery matches the full ownership tuple, leader id *and*
//! token: adopting a record on a token match alone would hand leadership to a
//! process that never won it.
//!
//! This is why a [`ClaimAttempt`] is created *before* `db.run` and passed
//! unchanged to every execution of the closure. It is also single-use. Once a
//! retry finds a foreign record at or above the ballot this attempt wrote, the
//! outcome is terminally [`ClaimOutcome::Superseded`]: the claim may have
//! committed and already been taken away, the process cannot know, so the token
//! is retired and a fresh campaign needs a fresh attempt.
//!
//! On FoundationDB 7.3 and later a caller may set
//! `DatabaseOption::TransactionAutomaticIdempotency` on its own database
//! handle, which makes the client resolve most unknown commits itself and so
//! makes retirement rare. Treat it as an optional layer on top, never as a
//! replacement for token recovery: it is a caller-side setting this recipe
//! never sets on your behalf, it does not exist below 7.3, it is still
//! experimental upstream, and it leaves the multiversion-client and
//! transaction-timeout paths able to produce an unknown result anyway. The
//! recipe's correctness may not depend on how the surrounding application
//! happens to be configured.
//!
//! # Fencing: the election service pattern
//!
//! An election alone is a liveness oracle. It says who *should* act; it cannot
//! stop anybody from acting. Safety comes from pairing it with a register that
//! rejects stale writers, which is the structure of Chockler and Malkhi's
//! *Active Disk Paxos with infinitely many processes* (PODC 2002): a leader
//! oracle plus a ranked register, with all the safety in the register. The
//! `ranked_register` recipe in this crate is that register, and the two
//! together are shipped as a documented composition rather than a wrapper type,
//! because which state a given deployment wants fenced is not something this
//! recipe can guess.
//!
//! The mechanics: `LeaseGrant::rank(sequence)` and `LeaseHandle::next_rank`
//! put the ballot in the high half of a rank and a
//! per-term sequence in the low half, so every rank of term `b + 1` dominates
//! every rank of term `b`. Ballots never reset, including across a resign,
//! which is the property that makes this sound and which the test suite pins
//! down. Handle clones mint ranks from one shared counter, and the sequence is
//! refused before it would wrap.
//!
//! **Activation is mandatory.** Winning ballot `b + 1` fences nothing by
//! itself. The register only starts rejecting the old leader once the new one
//! has installed its fence by calling `RankedRegister::read(grant.rank(0))`,
//! and until then a stale leader's writes may still land, exactly as with
//! Chubby sequencers. So: win the term, install the fence, then do or authorize
//! fenced work.
//!
//! **Activate in the claim's own transaction where you can.** At the primitive
//! layer the caller owns the transaction, so put
//! [`try_claim`](LeaderElection::try_claim) and the fencing
//! `RankedRegister::read(grant.rank(0))` in the same one. The term change and
//! the fence then commit together or not at all, and the unprotected window
//! disappears rather than merely being made short:
//!
//! ```text
//! db.run(|txn, _| async move {
//!     let (outcome, seen) = election.try_claim(&txn, id, lease, attempt, seen, now).await?;
//!     if let ClaimOutcome::Won(grant) = &outcome {
//!         register.read(&txn, grant.rank(0)).await?;   // same transaction
//!     }
//!     Ok((outcome, seen))
//! })
//! ```
//!
//! Activating immediately after the claim commits is the weaker fallback, and
//! it is what the handle layer gives you: [`LeaderElector::lead`] runs its
//! campaign in a transaction the caller does not compose into, so the fence can
//! only go in afterwards, as the first thing the work does. That is fine as
//! long as the work really does install it first. What it cannot survive is a
//! process that wins a term and then stops between the two steps: the register
//! keeps whatever fence it had, so a *previous* leader stays able to write to
//! it until somebody else wins a term and activates. A deterministic simulation
//! run found exactly that, and the workload now activates in the claim
//! transaction. The composition test and the simulation scenario both encode
//! the ordering. See the `ranked_register` module documentation for a worked
//! example at the handle layer.
//!
//! The guarantee is transactional, so it covers FoundationDB-resident state.
//! Effects outside the database get the ballot as a token, and the systems
//! receiving it have to enforce it themselves. A message broker that accepts
//! whatever it is sent is not fenced by anything this recipe can do.
//!
//! # Running work under a term
//!
//! [`LeaderElector::lead`] races the caller's work against the renewal driver
//! and the belief horizon, in one task. Lease maintenance is never a detached
//! thread: if the work stops being polled, renewals stop too, and the term
//! expires rather than being held by a process that is no longer running.
//!
//! **Cancellation.** Losing the term drops the work future. Dropping a future
//! stops the code at its next await point; it does not undo what already
//! happened, and it cannot stop a task the work spawned elsewhere. Two rules
//! follow, and neither is optional:
//!
//! - Fence effects with the ballot, per the section above. Cancellation is a
//!   best-effort stop signal, not a safety mechanism.
//! - Make work durable before announcing it. A successor must be able to pick
//!   up whatever the previous leader left half-done and redrive it
//!   idempotently, which means the record of "I am doing X" has to be committed
//!   before X becomes visible to anybody else.
//!
//! [`LeaseHandle`] is the token handed to the work. It is cloneable into every
//! task the work spawns, and it goes stale on its own clock, so a clone still
//! reports [`LeaseStatus::Lost`] after the elector that issued it is gone.
//! [`LeaseStatus::Jeopardy`] means a renewal could not be confirmed while the
//! horizon has not passed yet: the term is not lost, and work may continue
//! until the horizon.
//!
//! # Operating one
//!
//! **Shard the election.** One election per unit of work that can move
//! independently, each in its own [`Subspace`], rather than one global leader
//! for everything. It bounds the blast radius of a bad leader and of a slow
//! handover, and it spreads the load. The cost is more elections to observe.
//!
//! **Watch the leader's headroom.** How much work the current leader can absorb
//! is the application's metric, not this recipe's: an election is happy to keep
//! electing a leader that is falling behind. Track leader-side capacity
//! alongside leadership itself, and shard when the leader saturates.
//!
//! **Read the history.** [`LeaderElection::history`] returns the transition
//! trail, newest first, written in the same transactions as the transitions
//! themselves and ordered by commit versionstamp. Renewals are not recorded, so
//! it stays a rare-event log and answers "who was leader at time T" after the
//! fact. Retention is bounded and trimmed lazily by whoever writes next.
//!
//! **Choose a lease you can afford to wait out.** A crashed leader costs its
//! successor one lease of downtime, and renewals cost two transactions per
//! lease per leader. Shorter is more responsive and more expensive, and pushes
//! the safety margin closer to the scheduling noise floor.
//!
//! # Migrating from the registry-based recipe
//!
//! The storage format is a breaking change, and by design a loud one: records
//! written by the previous version of this recipe fail the schema-version check
//! and surface as [`LeaderElectionError::CorruptRecord`] rather than being
//! silently misread. Rolling mixed-version operation is not supported. The
//! upgrade is: drain and stop the old leaders, wait out one old lease, clear
//! the election subspace, deploy the new build.
//!
//! | Removed | Replacement |
//! |---|---|
//! | the candidate registry (`register_candidate`, `heartbeat_candidate`, `unregister_candidate`, `get_candidate`, `list_candidates`, `evict_dead_candidates`) | nothing to register: identity is the per-term [`ClaimToken`], liveness is the observation window, discovery is [`LeaderElection::leader`] plus [`LeaderElection::watch_term`] |
//! | stored configuration (`initialize`, `initialize_with_config`, `read_config`, `write_config`, `ElectionConfig`) | no configuration keys at all: the lease travels in the record, and [`ElectorConfig`] is client-side |
//! | `election_enabled` | stop running electors |
//! | priorities and `allow_preemption` | nothing: a term is taken only when its lease lapses or its holder resigns |
//! | `run_election_cycle`, `ElectionResult` | [`LeaderElector::lead`], or the primitives directly |
//! | `try_claim_leadership` | [`LeaderElection::try_claim`], with a [`ClaimAttempt`] and a [`LeaseObservation`] |
//! | `refresh_lease` | [`LeaderElection::refresh`] |
//! | `resign_leadership` | [`LeaderElection::resign`] |
//! | `is_leader` | [`LeaseHandle::check`] for the holder, [`LeaderElector::current_record`] for observers |
//! | `get_leader`, `get_leader_raw`, `LeaderState` | [`LeaderElection::leader`] returning a [`LeaderRecord`] |
//! | `ElectionDisabled`, `NotInitialized`, `ProcessNotFound`, `UnregisteredCandidate`, `InvalidState` | [`LeaderElectionError::CorruptRecord`], [`BallotExhausted`](LeaderElectionError::BallotExhausted), [`RankExhausted`](LeaderElectionError::RankExhausted), [`LeaseLost`](LeaderElectionError::LeaseLost), [`InvalidConfig`](LeaderElectionError::InvalidConfig), [`InvalidArgument`](LeaderElectionError::InvalidArgument) |
//!
//! Two behavioural changes deserve calling out. The ballot no longer resets
//! when leadership is released, so it is now usable as a fencing token, which
//! it was not before. And a restarted process inherits nothing from its
//! previous life: its old term carries a token it no longer has, so reusing a
//! `leader_id` after a restart is harmless rather than a way to bypass the
//! lease.
//!
//! # Lineage
//!
//! The previous version of this recipe followed Niazi, Ismail, Berthou and
//! Dowling, *Leader Election Using NewSQL Database Systems* (DAIS 2015, LNCS
//! 9038): a membership table of candidates with heartbeats, a leader chosen by
//! scanning it, and a monotonic id issued per leadership change.
//!
//! The departure is FoundationDB-specific. That design relies on shared and
//! exclusive lock modes to let readers scan the membership table without
//! fighting the writers heartbeating into it. FoundationDB has no lock modes:
//! under optimistic serializable concurrency, a scan of every candidate
//! conflicts with every concurrent heartbeat, so the scan-based election
//! degrades exactly when the cluster is busiest. The paper also has no fencing
//! story, which leaves the interesting failure mode unaddressed. Hence the
//! single contested record, and no registry.
//!
//! Four of its techniques survive, and are worth crediting:
//!
//! - anchoring the lease before the write is issued rather than at reply time
//!   (their `L_hbt`), here [`ClaimAttempt::started_at`];
//! - an explicit clock-drift allowance (their `mu`), here promoted to a stated
//!   rate contract, [`ElectorConfig::max_clock_rate_error`];
//! - the safety inequality that detection time must exceed lease time, here the
//!   observation window a steal has to complete;
//! - monotonically generated leadership ids, here the ballot, which this design
//!   additionally refuses to ever reset so that it can carry fencing.
//!
//! Alongside it: Chockler and Malkhi, *Active Disk Paxos with infinitely many
//! processes* (PODC 2002) for the register composition; Burrows, *The Chubby
//! lock service for loosely-coupled distributed systems* (OSDI 2006) for
//! sequencers, jeopardy and the release-versus-timeout asymmetry; Kleppmann,
//! *How to do distributed locking* (2016) for why fencing tokens are not
//! optional; and the AWS Builders' Library article on leader election in
//! distributed systems for the operational guidance above.
//!
//! Further reading:
//!
//! - Marc Brooker, "Leader election in distributed systems" (AWS Builders'
//!   Library), <https://aws.amazon.com/builders-library/leader-election-in-distributed-systems/>
//! - Martin Kleppmann, "How to do distributed locking" (2016),
//!   <https://martin.kleppmann.com/2016/02/08/how-to-do-distributed-locking.html>
//! - Burrows, "The Chubby lock service for loosely-coupled distributed
//!   systems" (OSDI 2006)
//! - Chockler and Malkhi, "Active Disk Paxos with infinitely many processes"
//!   (PODC 2002)
//! - Niazi, Ismail, Berthou, Dowling, "Leader Election Using NewSQL Database
//!   Systems" (DAIS 2015, LNCS 9038)
//! - awslabs/amazon-dynamodb-lock-client,
//!   <https://github.com/awslabs/amazon-dynamodb-lock-client>, for the
//!   clock-free lease-stealing pattern
//! - ZooKeeper recipes, including the leader election recipe and its GUID
//!   note, <https://zookeeper.apache.org/doc/r3.5.5/recipes.html>

mod codec;
mod decision;
mod elector;
mod errors;
mod types;

pub use elector::{
    DEFAULT_MAX_CLOCK_RATE_ERROR, DEFAULT_SCHEDULING_ALLOWANCE, ElectorConfig, JitterSchedule,
    LeadOutcome, LeaderElector, LeaseHandle, LeaseStatus, Timer, TokenSource,
};
#[cfg(feature = "recipes-leader-election-tokio")]
pub use elector::{TokioClock, TokioTimer};
pub use errors::{LeaderElectionError, LeaseLostError, Result};
pub use types::{
    ClaimAttempt, ClaimOutcome, ClaimToken, DEFAULT_HISTORY_RETENTION,
    DEFAULT_MAX_ADVERTISED_LEASE, HistoryEvent, HistoryEventKind, LeaderRecord, LeaseDuration,
    LeaseGrant, LeaseObservation, MAX_BALLOT, MAX_LEADER_ID_LEN, RecordIdentity, RefreshAttempt,
    RefreshOutcome, ResignOutcome, SCHEMA_VERSION,
};

use crate::options::{MutationType, StreamingMode};
use crate::{RangeOption, Transaction, tuple::Subspace};
use decision::{ClaimDecision, ClaimIdentity, RefreshDecision, ResignDecision};
use futures::TryStreamExt;
use futures::future::BoxFuture;
use std::ops::Deref;
use std::time::Duration;

/// Transaction-level leader election primitives
///
/// Every method here is one step of the protocol inside a caller-supplied
/// transaction, taking its notion of time as an argument. That makes the whole
/// layer deterministic and replay-safe, which is what lets `db.run` retry a
/// closure and what lets the deterministic simulator drive it.
///
/// Independent elections coexist by using different [`Subspace`]s.
///
/// # Thread Safety
///
/// [`Clone`], [`Send`] and [`Sync`]: it holds only a subspace and two
/// protocol constants.
#[derive(Clone, Debug)]
pub struct LeaderElection {
    subspace: Subspace,
    max_advertised_lease: Duration,
    history_retention: usize,
}

impl LeaderElection {
    /// Create an election over `subspace`
    ///
    /// No initialization step and no configuration keys: the only
    /// cross-process parameter is the lease, which each claimant advertises in
    /// the record itself.
    pub fn new(subspace: Subspace) -> Self {
        Self {
            subspace,
            max_advertised_lease: DEFAULT_MAX_ADVERTISED_LEASE,
            history_retention: DEFAULT_HISTORY_RETENTION,
        }
    }

    /// Set the ceiling on advertised leases
    ///
    /// Claims advertising more than this are rejected, and leases read from
    /// the record are clamped to it before being waited on. Every participant
    /// in one election must be configured with the same value: a contender
    /// with a lower ceiling will steal from a leader that still believes it
    /// leads.
    ///
    /// # Errors
    ///
    /// [`LeaderElectionError::InvalidConfig`] if the ceiling is zero.
    pub fn with_max_advertised_lease(mut self, max: Duration) -> Result<Self> {
        if max.is_zero() {
            return Err(LeaderElectionError::InvalidConfig(
                "max advertised lease must be non-zero".to_string(),
            ));
        }
        self.max_advertised_lease = max;
        Ok(self)
    }

    /// Set how many transitions the history subspace keeps
    ///
    /// Trimming is lazy: each writer drops entries beyond the bound in the
    /// transaction that appends its own, so the count is approximate at the
    /// margin. Zero disables the history entirely.
    pub fn with_history_retention(mut self, entries: usize) -> Self {
        self.history_retention = entries;
        self
    }

    /// The subspace this election lives in
    pub fn subspace(&self) -> &Subspace {
        &self.subspace
    }

    /// The ceiling on advertised leases
    pub fn max_advertised_lease(&self) -> Duration {
        self.max_advertised_lease
    }

    /// The key holding the contested record
    ///
    /// Exposed for composition. Watching this key wakes on every renewal;
    /// [`watch_term`](Self::watch_term) is almost always what you want
    /// instead.
    pub fn leader_key(&self) -> Vec<u8> {
        codec::leader_key(&self.subspace)
    }

    /// The key that moves only when leadership itself changes
    pub fn term_key(&self) -> Vec<u8> {
        codec::term_key(&self.subspace)
    }

    // ========================================================================
    // READS
    // ========================================================================

    /// Read the current record
    ///
    /// Returns `None` if the term was never claimed. A record that exists may
    /// still be vacant ([`LeaderRecord::is_vacant`]), and one that is occupied
    /// says nothing about whether its holder is alive: a single read cannot
    /// establish liveness.
    ///
    /// # Errors
    ///
    /// [`LeaderElectionError::CorruptRecord`] if the stored value is not a
    /// record this build understands.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip_all, err))]
    pub async fn leader<T>(&self, txn: &T) -> Result<Option<LeaderRecord>>
    where
        T: Deref<Target = Transaction>,
    {
        self.read_record(txn).await
    }

    /// Read the transition history, newest first
    ///
    /// The events are written in the same transactions as the transitions they
    /// describe, and keyed by commit versionstamp, so their order is exactly
    /// the commit order. Renewals are not recorded.
    ///
    /// At most `limit` events, and fewer than that only when the trail really
    /// is shorter: retention ([`with_history_retention`](Self::with_history_retention))
    /// trims the oldest entries, so a trail longer than the bound comes back as
    /// a suffix of the run rather than the whole of it.
    #[cfg_attr(
        feature = "trace",
        tracing::instrument(level = "debug", skip_all, fields(limit), err)
    )]
    pub async fn history<T>(&self, txn: &T, limit: usize) -> Result<Vec<HistoryEvent>>
    where
        T: Deref<Target = Transaction>,
    {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let opt = RangeOption {
            limit: Some(limit),
            reverse: true,
            mode: StreamingMode::WantAll,
            ..RangeOption::from(codec::history_subspace(&self.subspace).range())
        };

        // Paged to exhaustion, not read as one batch. `get_range` returns
        // whatever the first batch happened to hold, and for a reverse scan
        // that is a suffix of unpredictable length: two callers reading the
        // same trail in the same transaction can get different answers, and
        // neither is told it saw a partial one. A caller reasoning about an
        // audit trail cannot do anything useful with an arbitrary piece of it.
        let mut events = Vec::new();
        let mut stream = txn.get_ranges_keyvalues(opt, true);
        while let Some(kv) = stream.try_next().await? {
            events.push(codec::decode_history(&self.subspace, kv.key(), kv.value())?);
        }
        Ok(events)
    }

    /// Arm a watch on the term key
    ///
    /// The watch must be created in the same transaction as the read it is
    /// anchored to, and awaited only after that transaction has committed.
    /// Every result, including an error, is no more than a hint to re-read and
    /// re-arm: watches can coalesce, and a term that flaps back to its
    /// previous holder may produce no wake-up at all.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip_all))]
    pub fn watch_term<T>(&self, txn: &T) -> BoxFuture<'static, crate::FdbResult<()>>
    where
        T: Deref<Target = Transaction>,
    {
        #[cfg(feature = "trace")]
        tracing::debug!("watch armed on the term key");
        Box::pin(txn.watch(&self.term_key()))
    }

    // ========================================================================
    // CLAIM
    // ========================================================================

    /// Try to take a term
    ///
    /// `attempt` must be created before the enclosing `db.run` and passed
    /// unchanged to every execution of the closure: it anchors the lease
    /// before the write is issued and lets a retry recognize a claim of its
    /// own whose reply was lost. `observation` carries how long this caller
    /// has watched the record hold still, and the updated value must be
    /// threaded into the next call.
    ///
    /// `now` is invoked exactly once, immediately after the record is read, so
    /// that the observation window is measured against the read it belongs to.
    ///
    /// # Unknown commits
    ///
    /// A write is treated as possibly committed from the moment it is issued,
    /// so a retry that finds a foreign record at or above the ballot this
    /// attempt wrote returns [`ClaimOutcome::Superseded`] and retires the
    /// attempt. Callers on FoundationDB 7.3 and later may reduce how often that
    /// happens by setting `DatabaseOption::TransactionAutomaticIdempotency` on
    /// their own database handle, which lets the client resolve most unknown
    /// commits itself. It is an optional layer, not a substitute: this recipe
    /// never sets it, it does not exist below 7.3, it is still experimental
    /// upstream, and the multiversion-client and transaction-timeout paths can
    /// still produce an unknown result. The recovery contract here holds
    /// either way. See the [module documentation](self) for the full picture.
    ///
    /// # Errors
    ///
    /// - [`LeaderElectionError::InvalidArgument`] if `leader_id` is empty or
    ///   longer than [`MAX_LEADER_ID_LEN`], or if `lease` exceeds
    ///   [`max_advertised_lease`](Self::max_advertised_lease).
    /// - [`LeaderElectionError::BallotExhausted`] if the term counter has run
    ///   out.
    /// - [`LeaderElectionError::CorruptRecord`] on an undecodable record.
    #[cfg_attr(
        feature = "trace",
        tracing::instrument(
            level = "debug",
            skip_all,
            fields(leader_id = %leader_id, lease = %lease),
            err
        )
    )]
    pub async fn try_claim<T, F>(
        &self,
        txn: &T,
        leader_id: &str,
        lease: LeaseDuration,
        attempt: &ClaimAttempt,
        mut observation: LeaseObservation,
        now: F,
    ) -> Result<(ClaimOutcome, LeaseObservation)>
    where
        T: Deref<Target = Transaction>,
        F: FnOnce() -> Duration,
    {
        self.validate_leader_id(leader_id)?;
        if lease.as_duration() > self.max_advertised_lease {
            return Err(LeaderElectionError::InvalidArgument(format!(
                "lease {lease} exceeds the advertised maximum {:?}",
                self.max_advertised_lease
            )));
        }
        if attempt.is_retired() {
            #[cfg(feature = "trace")]
            tracing::warn!("claim refused: this attempt was already superseded");
            return Ok((ClaimOutcome::Superseded, observation));
        }

        let current = self.read_record(txn).await?;
        let now = now();

        let identity = ClaimIdentity {
            leader_id,
            token: attempt.token(),
            issued_ballot: attempt.issued_ballot(),
        };
        let decision = decision::decide_claim(
            current.as_ref(),
            &identity,
            now,
            self.max_advertised_lease,
            &mut observation,
        );

        let outcome = match decision {
            ClaimDecision::Claim { new_ballot, event } => {
                // Renewals continue the generation counter of the term they
                // follow, so identity moves forward on every applied write
                // regardless of which kind it was.
                let generation = current.as_ref().map_or(0, LeaderRecord::generation);
                let record = codec::claimed_record(
                    new_ballot,
                    generation,
                    leader_id,
                    attempt.token(),
                    lease,
                );

                // Recorded before the commit is attempted: if the reply is
                // lost, the retry needs to know a claim at this ballot may
                // already be in the database.
                attempt.note_issued(new_ballot);
                self.write_transition(txn, &record, event, leader_id)
                    .await?;

                #[cfg(feature = "trace")]
                tracing::info!(
                    ballot = new_ballot,
                    generation,
                    %event,
                    "leadership claimed"
                );
                ClaimOutcome::Won(LeaseGrant {
                    ballot: new_ballot,
                    generation,
                    leader_id: leader_id.to_string(),
                    token: attempt.token(),
                    lease,
                    acquired_at: attempt.started_at(),
                })
            }
            ClaimDecision::AlreadyWon => {
                // Our own record is already there. Adopting it rather than
                // writing again is what keeps a retried claim from consuming
                // two ballots.
                let record = current.expect("AlreadyWon implies a record was read");
                #[cfg(feature = "trace")]
                tracing::info!(
                    ballot = record.ballot(),
                    generation = record.generation(),
                    "leadership recovered from an unknown commit"
                );
                ClaimOutcome::Won(LeaseGrant {
                    ballot: record.ballot(),
                    generation: record.generation(),
                    leader_id: leader_id.to_string(),
                    token: attempt.token(),
                    lease: record.lease().expect("a held record advertises a lease"),
                    acquired_at: attempt.started_at(),
                })
            }
            ClaimDecision::Deny { remaining } => {
                let record = current.expect("Deny implies a record was read");
                #[cfg(feature = "trace")]
                tracing::debug!(
                    ballot = record.ballot(),
                    generation = record.generation(),
                    remaining_ms = remaining.as_millis() as u64,
                    "claim denied, the current term has not been still long enough"
                );
                ClaimOutcome::Denied {
                    current: record,
                    retry_after: remaining,
                }
            }
            ClaimDecision::Superseded => {
                attempt.retire();
                #[cfg(feature = "trace")]
                tracing::warn!(
                    issued_ballot = attempt.issued_ballot(),
                    "claim superseded: a write of this attempt may have committed and been taken over"
                );
                ClaimOutcome::Superseded
            }
            ClaimDecision::Exhausted => return Err(LeaderElectionError::BallotExhausted),
        };

        Ok((outcome, observation))
    }

    // ========================================================================
    // REFRESH
    // ========================================================================

    /// Extend a term by one generation
    ///
    /// A compare-and-set on the full ownership tuple that writes
    /// `generation + 1` at the same ballot, so the record's identity changes
    /// and every contender's observation window restarts. The term key is not
    /// touched, so renewals wake nobody.
    ///
    /// The returned grant is anchored at [`RefreshAttempt::issued_at`], never
    /// at the time the reply came back. Enforcing that the anchor has not
    /// already run out is the caller's job: this method reports what the
    /// database says, not what the caller may still believe.
    ///
    /// # Errors
    ///
    /// [`LeaderElectionError::CorruptRecord`] on an undecodable record.
    #[cfg_attr(
        feature = "trace",
        tracing::instrument(
            level = "debug",
            skip_all,
            fields(ballot = grant.ballot(), expected_generation = attempt.expected_generation()),
            err
        )
    )]
    pub async fn refresh<T>(
        &self,
        txn: &T,
        grant: &LeaseGrant,
        attempt: &RefreshAttempt,
    ) -> Result<RefreshOutcome>
    where
        T: Deref<Target = Transaction>,
    {
        let current = self.read_record(txn).await?;
        let decision =
            decision::decide_refresh(current.as_ref(), grant, attempt.expected_generation());

        let generation = match decision {
            RefreshDecision::Bump => {
                let generation = attempt.expected_generation() + 1;
                let record = codec::claimed_record(
                    grant.ballot(),
                    generation,
                    grant.leader_id(),
                    grant.token(),
                    grant.lease(),
                );
                txn.set(&self.leader_key(), &codec::encode_record(&record));
                #[cfg(feature = "trace")]
                tracing::debug!(generation, "lease renewed");
                generation
            }
            RefreshDecision::AlreadyBumped => {
                let generation = attempt.expected_generation() + 1;
                #[cfg(feature = "trace")]
                tracing::debug!(generation, "lease renewal recovered from an unknown commit");
                generation
            }
            RefreshDecision::Lost => {
                #[cfg(feature = "trace")]
                tracing::warn!(
                    observed_ballot = current.as_ref().map(LeaderRecord::ballot),
                    "lease lost: the record no longer matches this grant"
                );
                return Ok(RefreshOutcome::Lost { observed: current });
            }
        };

        Ok(RefreshOutcome::Refreshed(LeaseGrant {
            ballot: grant.ballot(),
            generation,
            leader_id: grant.leader_id().to_string(),
            token: grant.token(),
            lease: grant.lease(),
            acquired_at: attempt.issued_at(),
        }))
    }

    // ========================================================================
    // RESIGN
    // ========================================================================

    /// Give up a term
    ///
    /// Writes a vacant record that preserves the ballot, so the successor
    /// lands at `ballot + 1` with no observation wait at all. This asymmetry
    /// between an orderly handover and a crash is the point: only the holder
    /// can say for certain that it is done.
    ///
    /// The caller must already have stopped doing anything the term
    /// authorized. [`ResignOutcome::NotHolder`] may also mean an earlier
    /// resign of this term committed and the reply was lost; either way the
    /// caller stays stopped.
    ///
    /// # Errors
    ///
    /// [`LeaderElectionError::CorruptRecord`] on an undecodable record.
    #[cfg_attr(
        feature = "trace",
        tracing::instrument(level = "debug", skip_all, fields(ballot = grant.ballot()), err)
    )]
    pub async fn resign<T>(&self, txn: &T, grant: &LeaseGrant) -> Result<ResignOutcome>
    where
        T: Deref<Target = Transaction>,
    {
        let current = self.read_record(txn).await?;

        match decision::decide_resign(current.as_ref(), grant) {
            ResignDecision::Vacate => {
                let record = current.expect("Vacate implies a record was read");
                let vacated = codec::vacant_record(record.ballot(), record.generation());
                self.write_transition(txn, &vacated, HistoryEventKind::Resign, grant.leader_id())
                    .await?;
                #[cfg(feature = "trace")]
                tracing::info!(
                    ballot = record.ballot(),
                    "leadership resigned, the term is now vacant"
                );
                Ok(ResignOutcome::Resigned)
            }
            ResignDecision::AlreadyVacant => {
                #[cfg(feature = "trace")]
                tracing::info!("resign recovered: this term was already vacated by us");
                Ok(ResignOutcome::Resigned)
            }
            ResignDecision::NotHolder => {
                #[cfg(feature = "trace")]
                tracing::warn!(
                    observed_ballot = current.as_ref().map(LeaderRecord::ballot),
                    "resign refused: this term is not ours"
                );
                Ok(ResignOutcome::NotHolder)
            }
        }
    }

    // ========================================================================
    // INTERNALS
    // ========================================================================

    fn validate_leader_id(&self, leader_id: &str) -> Result<()> {
        if leader_id.is_empty() {
            return Err(LeaderElectionError::InvalidArgument(
                "leader id must not be empty: the empty id is the vacancy sentinel".to_string(),
            ));
        }
        if leader_id.len() > MAX_LEADER_ID_LEN {
            return Err(LeaderElectionError::InvalidArgument(format!(
                "leader id is {} bytes, the maximum is {MAX_LEADER_ID_LEN}",
                leader_id.len()
            )));
        }
        Ok(())
    }

    /// Read the record under a read conflict: this is the compare half of the
    /// compare-and-set that every transition relies on.
    async fn read_record<T>(&self, txn: &T) -> Result<Option<LeaderRecord>>
    where
        T: Deref<Target = Transaction>,
    {
        match txn.get(&self.leader_key(), false).await? {
            Some(bytes) => codec::decode_record(&bytes).map(Some),
            None => Ok(None),
        }
    }

    /// Write a leadership change: the record, the term marker watches park on,
    /// and the history entry, all in the caller's transaction so they commit
    /// together or not at all.
    async fn write_transition<T>(
        &self,
        txn: &T,
        record: &LeaderRecord,
        event: HistoryEventKind,
        leader_id: &str,
    ) -> Result<()>
    where
        T: Deref<Target = Transaction>,
    {
        txn.set(&self.leader_key(), &codec::encode_record(record));
        txn.set(&self.term_key(), &codec::encode_term(record));

        if self.history_retention > 0 {
            // Trim before appending: a key written with an incomplete
            // versionstamp is unreadable for the rest of the transaction, and a
            // range read covering it fails with `accessed_unreadable` (1036).
            self.trim_history(txn).await?;
            txn.atomic_op(
                &codec::incomplete_history_key(&self.subspace),
                &codec::encode_history(event, record.ballot(), leader_id),
                MutationType::SetVersionstampedKey,
            );
        }
        Ok(())
    }

    /// Drop history entries beyond the retention bound.
    ///
    /// The scan is a snapshot read: transitions already serialize on the
    /// leader key, so adding a conflict range over the history would only
    /// create contention without buying any ordering. The entry this
    /// transaction is appending has no key until commit and so is not counted,
    /// which is why the bound is approximate.
    async fn trim_history<T>(&self, txn: &T) -> Result<()>
    where
        T: Deref<Target = Transaction>,
    {
        let (begin, end) = codec::history_subspace(&self.subspace).range();
        let opt = RangeOption {
            limit: Some(self.history_retention),
            reverse: true,
            mode: StreamingMode::WantAll,
            ..RangeOption::from((begin.clone(), end))
        };
        let kvs = txn.get_range(&opt, 1, true).await?;
        if kvs.len() < self.history_retention {
            return Ok(());
        }
        if let Some(oldest_kept) = kvs.last() {
            txn.clear_range(&begin, oldest_kept.key());
        }
        Ok(())
    }
}
