// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Core data structures for leader election
//!
//! The stored leader record carries a two-part identity: a `ballot` that bumps
//! on every new term (claim or steal) and never resets, and a `generation`
//! that bumps on every lease renewal within a term. Every applied write
//! therefore changes `(ballot, generation)`, which is what lets contenders
//! measure "this record has not moved for a full lease" without ever comparing
//! wall clocks across processes.

use super::errors::{LeaderElectionError, Result};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

#[cfg(feature = "recipes-ranked-register")]
use crate::recipes::ranked_register::Rank;

/// Schema version of the stored leader record
///
/// Bumped whenever the record layout changes. A record carrying any other
/// version decodes to [`LeaderElectionError::CorruptRecord`].
pub const SCHEMA_VERSION: u64 = 1;

/// Default protocol-wide ceiling on an advertised lease duration
///
/// A claimant may not advertise a lease longer than this, and observers clamp
/// what they read to the same ceiling, so a misconfigured process cannot
/// sterilize an election for an unbounded time. Every participant must agree
/// on the value.
pub const DEFAULT_MAX_ADVERTISED_LEASE: Duration = Duration::from_secs(600);

/// Default number of leadership transitions kept in the history subspace
pub const DEFAULT_HISTORY_RETENTION: usize = 128;

/// Upper bound on the length of a leader identifier, in bytes
pub const MAX_LEADER_ID_LEN: usize = 256;

/// Highest ballot the protocol will ever hand out
///
/// Capped at `u32::MAX` so that `LeaseGrant::rank` can encode the ballot in
/// the high half of a ranked-register `Rank` without a fallible conversion.
pub const MAX_BALLOT: u64 = u32::MAX as u64;

// ============================================================================
// LEASE DURATION
// ============================================================================

/// A lease duration validated for the wire
///
/// Rejects zero (which is the vacancy sentinel in the stored record) and
/// durations whose nanosecond count does not fit a `u64`, so encoding never
/// truncates silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LeaseDuration(Duration);

impl LeaseDuration {
    /// Validate a lease duration
    ///
    /// # Errors
    ///
    /// [`LeaderElectionError::InvalidArgument`] if the duration is zero or
    /// does not fit in `u64` nanoseconds.
    pub fn new(duration: Duration) -> Result<Self> {
        if duration.is_zero() {
            return Err(LeaderElectionError::InvalidArgument(
                "lease duration must be non-zero".to_string(),
            ));
        }
        if u64::try_from(duration.as_nanos()).is_err() {
            return Err(LeaderElectionError::InvalidArgument(
                "lease duration does not fit in u64 nanoseconds".to_string(),
            ));
        }
        Ok(Self(duration))
    }

    /// The validated duration
    pub fn as_duration(self) -> Duration {
        self.0
    }

    /// The duration in nanoseconds, as stored in the record
    pub fn as_nanos(self) -> u64 {
        // `new` rejected anything that does not fit.
        self.0.as_nanos() as u64
    }

    /// Clamp this lease to a protocol-wide ceiling
    ///
    /// Applied by observers to whatever a record advertises, so an
    /// over-long advertised lease cannot delay a steal indefinitely.
    pub fn clamped_to(self, max: Duration) -> Duration {
        self.0.min(max)
    }
}

impl fmt::Display for LeaseDuration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

// ============================================================================
// CLAIM TOKEN
// ============================================================================

/// A per-term token identifying the process that claimed a term
///
/// The token is generated once per claim attempt, embedded in the record, and
/// never rotated within a term. Its only job is recovery: after a
/// `commit_unknown_result`, the retry recognizes its own successful write by
/// matching the full ownership tuple (leader id *and* token). Identity for
/// observation purposes is `(ballot, generation)`, not the token.
///
/// The all-zero token is the vacancy sentinel and is never a valid claim.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ClaimToken([u8; 16]);

impl ClaimToken {
    /// The all-zero token, used as the vacancy sentinel in stored records
    pub const ZERO: ClaimToken = ClaimToken([0u8; 16]);

    /// Build a token from caller-supplied bytes
    ///
    /// Dependency-free, so a deterministic simulation can feed tokens from its
    /// own seeded generator and keep runs reproducible.
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Generate a random token
    ///
    /// Convenience for production callers. Determinism-sensitive callers
    /// should use [`ClaimToken::from_bytes`] instead.
    pub fn generate() -> Self {
        Self(rand::random::<[u8; 16]>())
    }

    /// The raw token bytes
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// Whether this is the vacancy sentinel
    pub fn is_zero(&self) -> bool {
        self.0 == [0u8; 16]
    }
}

impl fmt::Debug for ClaimToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ClaimToken(")?;
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        write!(f, ")")
    }
}

// ============================================================================
// RECORD
// ============================================================================

/// The observable identity of a leader record
///
/// Every applied write changes this pair: claims and steals bump the ballot,
/// renewals bump the generation. Contenders time how long the identity has
/// stayed put; they never compare timestamps across processes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RecordIdentity {
    /// Term number, monotonic across the lifetime of the election subspace
    pub ballot: u64,
    /// Renewal counter within the term
    pub generation: u64,
}

/// The record stored at the leader key
///
/// Three states are representable: occupied (a process holds the term),
/// vacant (the term was resigned; the ballot is preserved so the successor
/// starts at `ballot + 1`), and absent (never claimed, represented by
/// `Option::None` at the call sites that read it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaderRecord {
    pub(crate) ballot: u64,
    pub(crate) generation: u64,
    pub(crate) leader_id: String,
    pub(crate) token: ClaimToken,
    pub(crate) lease_nanos: u64,
}

impl LeaderRecord {
    /// The term number
    pub fn ballot(&self) -> u64 {
        self.ballot
    }

    /// The renewal counter within the term
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The holder's identifier, or `None` if the record is vacant
    pub fn leader_id(&self) -> Option<&str> {
        if self.is_vacant() {
            None
        } else {
            Some(&self.leader_id)
        }
    }

    /// The holder's per-term token, [`ClaimToken::ZERO`] if vacant
    pub fn token(&self) -> ClaimToken {
        self.token
    }

    /// The lease this record advertises, or `None` if the record is vacant
    ///
    /// Observers must clamp this with
    /// [`LeaseDuration::clamped_to`] before using it as a wait.
    pub fn lease(&self) -> Option<LeaseDuration> {
        if self.is_vacant() {
            None
        } else {
            Some(LeaseDuration(Duration::from_nanos(self.lease_nanos)))
        }
    }

    /// Whether the term was resigned rather than held
    ///
    /// A vacant record can be reclaimed immediately: the previous holder said
    /// it was done, so no observation wait is owed.
    pub fn is_vacant(&self) -> bool {
        self.token.is_zero()
    }

    /// The `(ballot, generation)` pair observers track
    pub fn identity(&self) -> RecordIdentity {
        RecordIdentity {
            ballot: self.ballot,
            generation: self.generation,
        }
    }

    /// Whether this record is held by `leader_id` under `token`
    ///
    /// Matches the full ownership tuple: a token match alone is not enough to
    /// conclude "this is my record".
    pub fn is_held_by(&self, leader_id: &str, token: ClaimToken) -> bool {
        !self.is_vacant() && self.leader_id == leader_id && self.token == token
    }
}

// ============================================================================
// OBSERVATION
// ============================================================================

/// How long the caller has seen the leader record hold still
///
/// Threaded through successive [`try_claim`](super::LeaderElection::try_claim)
/// calls. The contents are deliberately opaque: they are only meaningful
/// against the caller's own monotonic clock, and comparing them across
/// processes would reintroduce the wall-clock defect this design removes.
///
/// A fresh observation ([`LeaseObservation::new`]) has seen nothing, so the
/// first call after it can never steal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LeaseObservation {
    identity: Option<RecordIdentity>,
    since: Duration,
}

impl LeaseObservation {
    /// A observation that has seen nothing yet
    pub fn new() -> Self {
        Self::default()
    }

    /// The identity currently being timed, if any
    pub fn observed_identity(&self) -> Option<RecordIdentity> {
        self.identity
    }

    /// When the current identity was first seen, on the caller's clock
    pub fn observed_since(&self) -> Option<Duration> {
        self.identity.map(|_| self.since)
    }

    /// Record a sighting of `identity` at `now`, returning how long it has
    /// held still.
    ///
    /// A different identity restarts the timer. A `now` that went backwards
    /// saturates the elapsed time to zero, which can only delay a steal, never
    /// enable one.
    pub(crate) fn note(&mut self, identity: RecordIdentity, now: Duration) -> Duration {
        if self.identity != Some(identity) {
            self.identity = Some(identity);
            self.since = now;
            return Duration::ZERO;
        }
        now.saturating_sub(self.since)
    }

    /// Forget what was being timed
    pub(crate) fn reset(&mut self) {
        self.identity = None;
        self.since = Duration::ZERO;
    }
}

// ============================================================================
// ATTEMPTS
// ============================================================================

/// A single-use anchor for one claim campaign
///
/// Created *before* `db.run` and held across the closure's retries, so that:
///
/// - the lease horizon is anchored before the write is issued, never at reply
///   time (a slow commit shortens the leader's belief, it never extends it);
/// - a retry after `commit_unknown_result` can recognize the record its own
///   earlier attempt may have written.
///
/// The attempt is single-use. Once a retry finds a foreign record at or above
/// the ballot this attempt wrote, the claim is terminally
/// [`Superseded`](ClaimOutcome::Superseded): the caller cannot know whether
/// its own write briefly landed, so the token is retired and a fresh campaign
/// needs a fresh attempt.
///
/// A write is treated as possibly committed from the moment it is issued,
/// without consulting the `MaybeCommitted` flag `db.run` hands the closure.
/// That flag describes the previous execution, while a closure can never
/// observe the fate of the commit it is itself issuing. The consequence is
/// deliberate and one-sided: a claim that lost a plain conflict, which
/// definitively did not commit, also retires its attempt. It costs one extra
/// transaction under contention, and it keeps the guarantee that a single
/// token can never account for two applied claims.
///
/// How often that costs anything can be reduced from the outside: see the note
/// on `DatabaseOption::TransactionAutomaticIdempotency` in
/// [`try_claim`](super::LeaderElection::try_claim). It is an optional layer on
/// top of this recovery, never a replacement for it.
#[derive(Debug)]
pub struct ClaimAttempt {
    token: ClaimToken,
    started_at: Duration,
    /// Ballot written by a previous execution of the closure, `0` for none.
    issued_ballot: AtomicU64,
    retired: std::sync::atomic::AtomicBool,
}

impl ClaimAttempt {
    /// Anchor a claim campaign
    ///
    /// `started_at` must come from the same monotonic timeline as the `now`
    /// passed to [`try_claim`](super::LeaderElection::try_claim).
    ///
    /// # Errors
    ///
    /// [`LeaderElectionError::InvalidArgument`] if the token is
    /// [`ClaimToken::ZERO`], which is reserved as the vacancy sentinel.
    pub fn new(token: ClaimToken, started_at: Duration) -> Result<Self> {
        if token.is_zero() {
            return Err(LeaderElectionError::InvalidArgument(
                "claim token must not be zero".to_string(),
            ));
        }
        Ok(Self {
            token,
            started_at,
            issued_ballot: AtomicU64::new(0),
            retired: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// The token this campaign writes into the record
    pub fn token(&self) -> ClaimToken {
        self.token
    }

    /// The pre-issuance anchor for the resulting lease
    pub fn started_at(&self) -> Duration {
        self.started_at
    }

    /// Whether a previous execution wrote a claim that may have committed
    pub fn maybe_committed(&self) -> bool {
        self.issued_ballot.load(Ordering::SeqCst) != 0
    }

    /// Whether this attempt has been terminally superseded
    pub fn is_retired(&self) -> bool {
        self.retired.load(Ordering::SeqCst)
    }

    pub(crate) fn issued_ballot(&self) -> Option<u64> {
        match self.issued_ballot.load(Ordering::SeqCst) {
            0 => None,
            b => Some(b),
        }
    }

    pub(crate) fn note_issued(&self, ballot: u64) {
        self.issued_ballot.store(ballot, Ordering::SeqCst);
    }

    pub(crate) fn retire(&self) {
        self.retired.store(true, Ordering::SeqCst);
    }
}

/// A pre-issuance anchor for one lease renewal
///
/// Like [`ClaimAttempt`], it is created before the transaction is issued and
/// retained across retries, so a renewal recovered from an unknown commit
/// keeps its original anchor instead of being re-anchored at reply time.
#[derive(Debug, Clone, Copy)]
pub struct RefreshAttempt {
    expected_generation: u64,
    issued_at: Duration,
}

impl RefreshAttempt {
    /// Anchor a renewal of `grant` at `issued_at`
    pub fn new(grant: &LeaseGrant, issued_at: Duration) -> Self {
        Self {
            expected_generation: grant.generation,
            issued_at,
        }
    }

    /// The generation this renewal expects to find in the record
    pub fn expected_generation(&self) -> u64 {
        self.expected_generation
    }

    /// The pre-issuance anchor for the renewed lease
    pub fn issued_at(&self) -> Duration {
        self.issued_at
    }
}

// ============================================================================
// GRANT
// ============================================================================

/// Proof that this process held the term at `acquired_at`
///
/// The grant carries no live notion of validity: whether the holder may still
/// *believe* it leads is a decision the caller makes by comparing its own
/// clock against [`LeaseGrant::expires_at`], minus whatever safety margin its
/// clock-rate assumption requires. The handle layer does this; callers using
/// the primitives directly must do it themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseGrant {
    pub(crate) ballot: u64,
    pub(crate) generation: u64,
    pub(crate) leader_id: String,
    pub(crate) token: ClaimToken,
    pub(crate) lease: LeaseDuration,
    pub(crate) acquired_at: Duration,
}

impl LeaseGrant {
    /// The term number, monotonic and never reset
    ///
    /// This is the fencing token. See the [module documentation](super) for
    /// how it composes with the ranked register.
    pub fn ballot(&self) -> u64 {
        self.ballot
    }

    /// The renewal counter at the time this grant was issued
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The identifier this process claimed under
    pub fn leader_id(&self) -> &str {
        &self.leader_id
    }

    /// The per-term token, fixed for the whole term
    pub fn token(&self) -> ClaimToken {
        self.token
    }

    /// The lease this grant advertises
    pub fn lease(&self) -> LeaseDuration {
        self.lease
    }

    /// The pre-issuance anchor, on the caller's monotonic clock
    pub fn acquired_at(&self) -> Duration {
        self.acquired_at
    }

    /// When the advertised lease runs out, on the caller's monotonic clock
    ///
    /// This is the outer bound, not a safe belief horizon: a leader must stop
    /// believing strictly earlier, by a margin covering its clock-rate error
    /// against the contender's.
    pub fn expires_at(&self) -> Duration {
        self.acquired_at
            .checked_add(self.lease.as_duration())
            .unwrap_or(Duration::MAX)
    }

    /// A fencing rank for this term
    ///
    /// The ballot occupies the high half and `sequence` the low half, so every
    /// rank of term `b + 1` dominates every rank of term `b`. Infallible: the
    /// protocol refuses to hand out a ballot above [`MAX_BALLOT`].
    ///
    /// Winning a term does not by itself fence anything. A new leader must
    /// install its fence with `RankedRegister::read(grant.rank(0))` before
    /// doing or authorizing any fenced work.
    #[cfg(feature = "recipes-ranked-register")]
    pub fn rank(&self, sequence: u32) -> Rank {
        debug_assert!(self.ballot <= MAX_BALLOT, "ballot exceeds the rank cap");
        Rank::new(sequence, self.ballot as u32)
    }
}

// ============================================================================
// OUTCOMES
// ============================================================================

/// What a [`try_claim`](super::LeaderElection::try_claim) transaction decided
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// The term was won; the transaction wrote the new record
    Won(LeaseGrant),
    /// Another process holds a term that has not been still long enough
    Denied {
        /// The record that was observed
        current: LeaderRecord,
        /// How much longer the observation must run before a steal is allowed
        retry_after: Duration,
    },
    /// The attempt is terminally spent
    ///
    /// A previous execution of this attempt may have committed a claim, and
    /// the record has since moved past it. Whether this process was ever
    /// leader is unknowable; it must not act as one. Start a fresh
    /// [`ClaimAttempt`] with a fresh token to campaign again.
    Superseded,
}

/// What a [`refresh`](super::LeaderElection::refresh) transaction decided
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// The lease was renewed; the grant is re-anchored at the renewal's
    /// pre-issuance time
    Refreshed(LeaseGrant),
    /// The term is gone
    Lost {
        /// Whatever occupies the leader key now, if anything
        observed: Option<LeaderRecord>,
    },
}

/// What a [`resign`](super::LeaderElection::resign) transaction decided
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResignOutcome {
    /// The term is now vacant and can be reclaimed without any wait
    Resigned,
    /// This process does not hold the term
    ///
    /// May also mean an earlier resign of this same term committed and the
    /// term has since moved on. Either way the caller stays stopped.
    NotHolder,
}

// ============================================================================
// HISTORY
// ============================================================================

/// The kind of leadership transition recorded in the history subspace
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HistoryEventKind {
    /// A term was taken over an absent or vacant record
    Claim,
    /// A term was taken from a holder whose lease was observed to expire
    Steal,
    /// A term was given up
    Resign,
}

impl HistoryEventKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Claim => "claim",
            Self::Steal => "steal",
            Self::Resign => "resign",
        }
    }

    pub(crate) fn from_str(value: &str) -> Option<Self> {
        match value {
            "claim" => Some(Self::Claim),
            "steal" => Some(Self::Steal),
            "resign" => Some(Self::Resign),
            _ => None,
        }
    }
}

impl fmt::Display for HistoryEventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One leadership transition, written in the same transaction as the
/// transition itself
///
/// Renewals are not recorded: the log is a rare-event audit trail, not a
/// heartbeat stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEvent {
    pub(crate) versionstamp: [u8; 12],
    pub(crate) kind: HistoryEventKind,
    pub(crate) ballot: u64,
    pub(crate) leader_id: String,
}

impl HistoryEvent {
    /// The commit versionstamp, which orders events exactly as they committed
    pub fn versionstamp(&self) -> [u8; 12] {
        self.versionstamp
    }

    /// What happened
    pub fn kind(&self) -> HistoryEventKind {
        self.kind
    }

    /// The ballot the transition produced
    pub fn ballot(&self) -> u64 {
        self.ballot
    }

    /// Who caused the transition
    pub fn leader_id(&self) -> &str {
        &self.leader_id
    }
}
