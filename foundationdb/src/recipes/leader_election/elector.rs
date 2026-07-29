// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! The async handle layer
//!
//! [`LeaderElection`] is one protocol step per call and knows nothing about
//! time passing. This module is the loop around it: campaign until a term is
//! won, run the caller's work while renewing the lease in the same task, and
//! stop believing strictly before any contender could take over.
//!
//! Time is read through [`Clock`] and waiting is done through [`Timer`], both
//! handed in by the caller, so the whole layer can run on a simulated timeline.
//! Nothing here reads an ambient clock or draws ambient randomness: the
//! campaign jitter and the per-term claim tokens come from the [`Environment`]
//! unless the config names its own.
//!
//! Only [`Clock::monotonic`] is ever read. Everything this layer measures is an
//! elapsed duration on one instance, which is exactly what monotonic readings
//! are for; nothing it computes is persisted or compared across processes, so
//! [`Clock::wall`] has no use here.
//!
//! [`Environment::default`] is the production choice. A seeded environment
//! ([`Environment::with_seed`]) makes a campaign replay identically: the jitter
//! schedule and every claim token come out of that one seed.
//!
//! # Belief
//!
//! A grant says "the database accepted my claim at `acquired_at`". Turning that
//! into "I may still act as leader" needs an assumption about clocks, and this
//! layer makes it explicit: every participant's clock runs within
//! [`max_clock_rate_error`](ElectorConfig::max_clock_rate_error) of real time.
//! From that bound the config derives a safety margin, and the leader hard-stops
//! at `acquired_at + lease - margin` no matter what the renewal loop is doing.
//! Contenders wait a full lease measured on their own clock, so the leader's
//! belief always ends first. See [`ElectorConfig::safety_margin`] for the
//! derivation.
//!
//! Offsets between clocks are irrelevant: only elapsed durations are ever
//! compared, and always on the clock that measured them.

use super::errors::{LeaderElectionError, LeaseLostError, Result};
use super::types::{
    ClaimAttempt, ClaimOutcome, ClaimToken, LeaderRecord, LeaseDuration, LeaseGrant,
    LeaseObservation, RefreshAttempt, RefreshOutcome, ResignOutcome,
};
use super::{LeaderElection, MAX_LEADER_ID_LEN};
use crate::env::{Clock, Environment, Rng};
use crate::{Database, FdbResult, tuple::Subspace};
use futures::future::{BoxFuture, Either};
use std::fmt;
use std::future::Future;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

#[cfg(feature = "recipes-ranked-register")]
use crate::recipes::ranked_register::Rank;
#[cfg(feature = "recipes-ranked-register")]
use std::sync::atomic::AtomicU32;

/// Default bound on how far any participant's clock rate may drift from real
/// time: one millisecond per second.
pub const DEFAULT_MAX_CLOCK_RATE_ERROR: f64 = 1e-3;

/// Default allowance added to the derived safety margin to cover scheduling
/// delays: timer slop, a garbage collection pause, a busy executor.
pub const DEFAULT_SCHEDULING_ALLOWANCE: Duration = Duration::from_millis(50);

// ============================================================================
// TIMER
// ============================================================================

/// Waiting, as this layer does it
///
/// The counterpart of [`Clock`]: the clock says what time it is, this says how
/// to wait for more of it. It is a separate trait because [`Environment`]
/// deliberately supplies values only, never control over execution, so a
/// caller running on a simulated timeline pairs the simulator's clock with a
/// timer driving the simulator's own schedule.
///
/// `TokioTimer` (feature `recipes-leader-election-tokio`) is the production
/// implementation.
pub trait Timer: fmt::Debug + Send + Sync {
    /// A future that completes no earlier than `duration` from now
    fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()>;
}

/// A [`Clock`] over the tokio timeline
///
/// Requires the `recipes-leader-election-tokio` feature.
/// [`monotonic`](Clock::monotonic) counts from the moment the instance was
/// built and reads `tokio::time::Instant`, so under `tokio::time::pause()` it
/// follows the paused timeline and tokio's own time control works on the
/// elector unchanged. [`wall`](Clock::wall) is the machine clock, as in
/// [`WallClock`](crate::env::WallClock), and the elector never reads it.
#[cfg(feature = "recipes-leader-election-tokio")]
#[derive(Debug, Clone)]
pub struct TokioClock {
    epoch: tokio::time::Instant,
}

#[cfg(feature = "recipes-leader-election-tokio")]
impl TokioClock {
    /// Start a clock whose monotonic epoch is now
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug"))]
    pub fn new() -> Self {
        Self {
            epoch: tokio::time::Instant::now(),
        }
    }
}

#[cfg(feature = "recipes-leader-election-tokio")]
impl Default for TokioClock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "recipes-leader-election-tokio")]
impl Clock for TokioClock {
    fn monotonic(&self) -> Duration {
        self.epoch.elapsed()
    }

    fn wall(&self) -> Duration {
        // A system clock set before 1970 is the only way this fails, and no
        // caller can do anything useful with an error here.
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
    }
}

/// A [`Timer`] backed by the tokio runtime
///
/// Requires the `recipes-leader-election-tokio` feature and a runtime with the
/// time driver enabled. Pairs with [`TokioClock`], including under
/// `tokio::time::pause()`.
#[cfg(feature = "recipes-leader-election-tokio")]
#[derive(Debug, Clone, Copy, Default)]
pub struct TokioTimer;

#[cfg(feature = "recipes-leader-election-tokio")]
impl Timer for TokioTimer {
    fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()> {
        Box::pin(tokio::time::sleep(duration))
    }
}

// ============================================================================
// DETERMINISM HOOKS
// ============================================================================

/// Where a campaign gets its per-term tokens
///
/// An elector that is not given one draws its tokens from the [`Rng`] of its
/// [`Environment`], so the campaign path holds no ambient randomness. Set one
/// explicitly to take the tokens from somewhere else entirely.
#[derive(Clone)]
pub struct TokenSource(Arc<dyn Fn() -> ClaimToken + Send + Sync>);

impl TokenSource {
    /// Build a source from a closure
    pub fn new<F>(source: F) -> Self
    where
        F: Fn() -> ClaimToken + Send + Sync + 'static,
    {
        Self(Arc::new(source))
    }

    /// Issue the token for one campaign attempt
    pub fn issue(&self) -> ClaimToken {
        (self.0)()
    }
}

impl fmt::Debug for TokenSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenSource").finish_non_exhaustive()
    }
}

/// How denied contenders spread themselves out
///
/// Contenders denied by the same record wake at the same moment, so the
/// campaign adds a delay drawn from `[0, window)` before retrying. The draw is
/// a pure function of `(seed, round)`, so a run with a fixed seed replays
/// identically; an elector that is not given a schedule draws its seed from the
/// [`Rng`] of its [`Environment`], which is what keeps two processes with the
/// same settings from marching in lockstep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JitterSchedule {
    seed: u64,
    window: Duration,
}

impl JitterSchedule {
    /// A schedule drawing from `[0, window)`
    pub const fn new(seed: u64, window: Duration) -> Self {
        Self { seed, window }
    }

    /// The delay to add after campaign round `round`
    pub fn jitter_for(&self, round: u64) -> Duration {
        if self.window.is_zero() {
            return Duration::ZERO;
        }
        // SplitMix64 finalizer: a pure, dependency-free mix so the schedule is
        // reproducible across builds and platforms.
        let mut z = self
            .seed
            .wrapping_add(round.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        let nanos = u64::try_from(self.window.as_nanos()).unwrap_or(u64::MAX);
        Duration::from_nanos(z % nanos)
    }
}

// ============================================================================
// CONFIG
// ============================================================================

/// How an elector spends its lease
///
/// Built from one number, the lease duration, with everything else derived and
/// overridable. Every setter revalidates, so a config that exists is a config
/// whose renewals fit inside its lease.
#[derive(Debug, Clone)]
pub struct ElectorConfig {
    lease: LeaseDuration,
    renew_interval: Duration,
    max_clock_rate_error: f64,
    scheduling_allowance: Duration,
    safety_margin: Duration,
    backoff_cap: Duration,
    refresh_timeout: Duration,
    jeopardy_backoff: Duration,
    resign_timeout: Duration,
    /// `None` until an elector resolves it against its environment.
    jitter: Option<JitterSchedule>,
    /// `None` until an elector resolves it against its environment.
    token_source: Option<TokenSource>,
}

impl ElectorConfig {
    /// Derive a config from a lease duration
    ///
    /// Defaults: renew every `lease / 3` (two renewals per lease, so one lost
    /// transaction is not fatal), campaign backoff capped at the lease, a
    /// renewal transaction budget of one renew interval, and the safety margin
    /// described in [`safety_margin`](Self::safety_margin).
    ///
    /// The campaign jitter and the source of claim tokens are left unset: an
    /// elector resolves them against its [`Environment`] unless
    /// [`with_jitter`](Self::with_jitter) or
    /// [`with_token_source`](Self::with_token_source) name one.
    ///
    /// # Errors
    ///
    /// [`LeaderElectionError::InvalidArgument`] if the lease is zero or does
    /// not fit in `u64` nanoseconds, [`LeaderElectionError::InvalidConfig`] if
    /// the derived values do not fit inside it (a lease of a few nanoseconds).
    pub fn new(lease: Duration) -> Result<Self> {
        let lease = LeaseDuration::new(lease)?;
        let renew_interval = lease.as_duration() / 3;
        let config = Self {
            lease,
            renew_interval,
            max_clock_rate_error: DEFAULT_MAX_CLOCK_RATE_ERROR,
            scheduling_allowance: DEFAULT_SCHEDULING_ALLOWANCE,
            // Replaced by `revalidate` before the config escapes.
            safety_margin: Duration::ZERO,
            backoff_cap: lease.as_duration(),
            refresh_timeout: renew_interval,
            jeopardy_backoff: renew_interval / 4,
            resign_timeout: renew_interval,
            jitter: None,
            token_source: None,
        };
        config.revalidate()
    }

    /// How often the lease is renewed
    ///
    /// # Errors
    ///
    /// [`LeaderElectionError::InvalidConfig`] if it is zero, longer than half
    /// the lease, or leaves no room for the safety margin.
    pub fn with_renew_interval(mut self, interval: Duration) -> Result<Self> {
        self.renew_interval = interval;
        self.revalidate()
    }

    /// The assumed bound on clock rate error, as a fraction of real time
    ///
    /// `1e-3` means every participant's clock is assumed to run within one
    /// millisecond per second of real time. Raising it widens the safety
    /// margin, which shortens the useful part of every lease.
    ///
    /// # Errors
    ///
    /// [`LeaderElectionError::InvalidConfig`] if the value is not a finite
    /// number in `[0, 1)`, or if the resulting margin does not fit in the
    /// lease.
    pub fn with_max_clock_rate_error(mut self, error: f64) -> Result<Self> {
        self.max_clock_rate_error = error;
        self.revalidate()
    }

    /// Extra margin covering scheduling delays rather than clock error
    ///
    /// Added on top of the rate-derived margin. Set it to zero only when the
    /// timeline is under your control, as in a simulation.
    ///
    /// # Errors
    ///
    /// [`LeaderElectionError::InvalidConfig`] if the resulting margin does not
    /// fit in the lease.
    pub fn with_scheduling_allowance(mut self, allowance: Duration) -> Result<Self> {
        self.scheduling_allowance = allowance;
        self.revalidate()
    }

    /// The longest a denied contender waits before re-reading
    ///
    /// A denial reports exactly how much observation time is still owed, so the
    /// cap only matters when a record advertises a long lease.
    ///
    /// # Errors
    ///
    /// [`LeaderElectionError::InvalidConfig`] if the cap is zero.
    pub fn with_backoff_cap(mut self, cap: Duration) -> Result<Self> {
        self.backoff_cap = cap;
        self.revalidate()
    }

    /// How long one renewal may take before it is abandoned
    ///
    /// Clamped at runtime by the belief horizon: a renewal never gets to spend
    /// time the leader no longer has.
    ///
    /// # Errors
    ///
    /// [`LeaderElectionError::InvalidConfig`] if the timeout is zero.
    pub fn with_refresh_timeout(mut self, timeout: Duration) -> Result<Self> {
        self.refresh_timeout = timeout;
        self.revalidate()
    }

    /// How long to wait between renewal attempts while in jeopardy
    ///
    /// # Errors
    ///
    /// [`LeaderElectionError::InvalidConfig`] if the backoff is zero, which
    /// would spin.
    pub fn with_jeopardy_backoff(mut self, backoff: Duration) -> Result<Self> {
        self.jeopardy_backoff = backoff;
        self.revalidate()
    }

    /// How long the best-effort resign after the work completes may take
    ///
    /// # Errors
    ///
    /// [`LeaderElectionError::InvalidConfig`] if the timeout is zero.
    pub fn with_resign_timeout(mut self, timeout: Duration) -> Result<Self> {
        self.resign_timeout = timeout;
        self.revalidate()
    }

    /// Pin the campaign jitter schedule instead of deriving one from the
    /// elector's [`Environment`]
    pub fn with_jitter(mut self, jitter: JitterSchedule) -> Self {
        self.jitter = Some(jitter);
        self
    }

    /// Take per-term claim tokens from `source` instead of from the elector's
    /// [`Environment`]
    pub fn with_token_source(mut self, source: TokenSource) -> Self {
        self.token_source = Some(source);
        self
    }

    /// The lease every claim of this elector advertises
    pub fn lease(&self) -> LeaseDuration {
        self.lease
    }

    /// How often the lease is renewed
    pub fn renew_interval(&self) -> Duration {
        self.renew_interval
    }

    /// The assumed bound on clock rate error
    pub fn max_clock_rate_error(&self) -> f64 {
        self.max_clock_rate_error
    }

    /// How much of the lease is given up to clock error and scheduling
    ///
    /// The leader stops believing at `acquired_at + lease - margin`, and
    ///
    /// ```text
    /// margin = lease * 2e / (1 + e)  +  scheduling allowance
    /// ```
    ///
    /// with `e` the assumed clock rate bound. The rate term is what makes the
    /// belief horizon safe. Take a leader whose clock is as slow as the bound
    /// allows and a contender whose clock is as fast as the bound allows, and
    /// measure both against real time:
    ///
    /// - the leader stops after `(lease - margin) * (1 + e)` of real time,
    ///   which with the formula above is exactly `lease * (1 - e)`;
    /// - the contender cannot start timing the record before the leader's write
    ///   committed, and needs a full lease on its own fast clock, so it steals
    ///   no earlier than `lease / (1 + e)` of real time after that.
    ///
    /// Since `(1 - e)(1 + e) = 1 - e² <= 1`, the leader always stops first, with
    /// `e² * lease` to spare. The scheduling allowance is on top of that, for
    /// the part of the delay that is not clock error at all.
    pub fn safety_margin(&self) -> Duration {
        self.safety_margin
    }

    /// The extra margin covering scheduling delays
    pub fn scheduling_allowance(&self) -> Duration {
        self.scheduling_allowance
    }

    /// The longest a denied contender waits before re-reading
    pub fn backoff_cap(&self) -> Duration {
        self.backoff_cap
    }

    /// How long one renewal may take before it is abandoned
    pub fn refresh_timeout(&self) -> Duration {
        self.refresh_timeout
    }

    /// How long to wait between renewal attempts while in jeopardy
    pub fn jeopardy_backoff(&self) -> Duration {
        self.jeopardy_backoff
    }

    /// How long the best-effort resign may take
    pub fn resign_timeout(&self) -> Duration {
        self.resign_timeout
    }

    /// The pinned campaign jitter schedule, `None` when an elector should
    /// derive one from its [`Environment`]
    pub fn jitter(&self) -> Option<JitterSchedule> {
        self.jitter
    }

    /// The pinned source of per-term claim tokens, `None` when an elector
    /// should draw them from its [`Environment`]
    pub fn token_source(&self) -> Option<&TokenSource> {
        self.token_source.as_ref()
    }

    /// Recompute the derived margin and check that the whole schedule fits
    /// inside one lease.
    fn revalidate(mut self) -> Result<Self> {
        let lease = self.lease.as_duration();

        if !self.max_clock_rate_error.is_finite()
            || self.max_clock_rate_error < 0.0
            || self.max_clock_rate_error >= 1.0
        {
            return Err(LeaderElectionError::InvalidConfig(format!(
                "max clock rate error must be a finite fraction in [0, 1), got {}",
                self.max_clock_rate_error
            )));
        }
        if self.renew_interval.is_zero() {
            return Err(LeaderElectionError::InvalidConfig(
                "renew interval must be non-zero".to_string(),
            ));
        }
        // The DynamoDB lock client rule: at least two renewals per lease, so a
        // single failed renewal is not immediately fatal.
        if self.renew_interval * 2 > lease {
            return Err(LeaderElectionError::InvalidConfig(format!(
                "renew interval {:?} is more than half the lease {:?}",
                self.renew_interval, lease
            )));
        }
        for (name, value) in [
            ("backoff cap", self.backoff_cap),
            ("refresh timeout", self.refresh_timeout),
            ("jeopardy backoff", self.jeopardy_backoff),
            ("resign timeout", self.resign_timeout),
        ] {
            if value.is_zero() {
                return Err(LeaderElectionError::InvalidConfig(format!(
                    "{name} must be non-zero"
                )));
            }
        }

        let e = self.max_clock_rate_error;
        let rate_margin = lease.mul_f64(2.0 * e / (1.0 + e));
        self.safety_margin = rate_margin.saturating_add(self.scheduling_allowance);

        if self.renew_interval.saturating_add(self.safety_margin) >= lease {
            return Err(LeaderElectionError::InvalidConfig(format!(
                "renew interval {:?} plus safety margin {:?} does not fit in the lease {:?}",
                self.renew_interval, self.safety_margin, lease
            )));
        }
        Ok(self)
    }
}

// ============================================================================
// PURE HORIZON ARITHMETIC
// ============================================================================

/// The instant, on the holder's own clock, after which it must stop believing
/// it leads.
///
/// Pure so the exact boundary can be tested without a database or a timeline.
pub(crate) fn belief_horizon(acquired_at: Duration, lease: Duration, margin: Duration) -> Duration {
    acquired_at.saturating_add(lease.saturating_sub(margin))
}

/// Whether a renewal result that just came back may still be applied.
///
/// Called after resampling the clock, immediately before the result is used: a
/// renewal that was in flight while the horizon passed is discarded, because by
/// then a contender may already have started counting. The boundary is
/// exclusive, so a result landing exactly at the horizon is too late.
pub(crate) fn refresh_still_applies(now: Duration, horizon: Duration) -> bool {
    now < horizon
}

// ============================================================================
// HANDLE
// ============================================================================

const STATUS_LEADING: u8 = 0;
const STATUS_JEOPARDY: u8 = 1;
const STATUS_LOST: u8 = 2;

/// What a [`LeaseHandle`] currently believes
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LeaseStatus {
    /// The lease was renewed recently and the horizon has not passed
    Leading,
    /// A renewal could not be completed, but the horizon has not passed yet
    ///
    /// Chubby's jeopardy: the term is not lost, the holder just cannot confirm
    /// it. Work may continue until the horizon, and a renewal that succeeds
    /// before then returns the handle to [`Leading`](Self::Leading).
    Jeopardy,
    /// The term is gone, or the horizon passed. Terminal.
    Lost,
}

/// State shared by every clone of one term's handle.
#[derive(Debug)]
struct LeaseState {
    ballot: u64,
    status: AtomicU8,
    /// Belief horizon in nanoseconds on the shared clock, moved forward by
    /// each applied renewal.
    horizon_nanos: AtomicU64,
    /// The next fencing sequence, shared by every clone so two clones can never
    /// mint the same rank.
    #[cfg(feature = "recipes-ranked-register")]
    sequence: AtomicU32,
    clock: Arc<dyn Clock>,
}

impl LeaseState {
    fn new(ballot: u64, horizon: Duration, clock: Arc<dyn Clock>) -> Self {
        Self {
            ballot,
            status: AtomicU8::new(STATUS_LEADING),
            horizon_nanos: AtomicU64::new(nanos_of(horizon)),
            #[cfg(feature = "recipes-ranked-register")]
            sequence: AtomicU32::new(0),
            clock,
        }
    }

    fn horizon(&self) -> Duration {
        Duration::from_nanos(self.horizon_nanos.load(Ordering::Acquire))
    }

    fn set_horizon(&self, horizon: Duration) {
        self.horizon_nanos
            .store(nanos_of(horizon), Ordering::Release);
    }

    fn mark_lost(&self) {
        self.status.store(STATUS_LOST, Ordering::Release);
    }

    /// Move to `next` unless the term is already lost: losing it is terminal,
    /// and a late success must never resurrect a handle a caller has already
    /// been told to stop trusting.
    fn set_status(&self, next: u8) {
        let _ = self
            .status
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                if current == STATUS_LOST {
                    None
                } else {
                    Some(next)
                }
            });
    }

    fn status(&self) -> LeaseStatus {
        match self.status.load(Ordering::Acquire) {
            STATUS_LOST => LeaseStatus::Lost,
            raw => {
                // The fallback that makes the handle honest even if the elector
                // future was dropped, or the task driving it never runs again:
                // nobody has to tell us the term expired, the clock does.
                if self.clock.monotonic() >= self.horizon() {
                    self.mark_lost();
                    return LeaseStatus::Lost;
                }
                if raw == STATUS_JEOPARDY {
                    LeaseStatus::Jeopardy
                } else {
                    LeaseStatus::Leading
                }
            }
        }
    }
}

fn nanos_of(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

/// A leadership token, in the Chubby sequencer sense
///
/// Handed to the work closure and cloneable into every task that work spawns.
/// It carries the ballot to fence with, and it goes stale on its own: once the
/// belief horizon passes, every clone reports [`LeaseStatus::Lost`] even if the
/// elector that issued it is gone.
///
/// A handle is not a lock. It tells you what this process is entitled to
/// believe; enforcing that belief is the job of the fencing composition
/// documented on `LeaseGrant::rank`.
#[derive(Debug, Clone)]
pub struct LeaseHandle {
    state: Arc<LeaseState>,
}

impl LeaseHandle {
    /// The term this handle was issued for
    ///
    /// Fixed for the life of the handle: renewals extend a term, they never
    /// start a new one. This is the fencing token.
    pub fn ballot(&self) -> u64 {
        self.state.ballot
    }

    /// What this handle currently believes
    #[cfg_attr(feature = "trace", tracing::instrument(level = "trace", skip_all))]
    pub fn status(&self) -> LeaseStatus {
        self.state.status()
    }

    /// Fail if the term can no longer be trusted
    ///
    /// [`LeaseStatus::Jeopardy`] passes: the lease has not run out, the holder
    /// merely could not confirm it. Only the horizon, or an observed takeover,
    /// makes this fail.
    ///
    /// # Errors
    ///
    /// [`LeaseLostError`] once the term is [`LeaseStatus::Lost`].
    pub fn check(&self) -> std::result::Result<(), LeaseLostError> {
        match self.status() {
            LeaseStatus::Lost => Err(LeaseLostError {
                ballot: self.state.ballot,
            }),
            _ => Ok(()),
        }
    }

    /// The next fencing rank of this term
    ///
    /// Ranks are minted from one counter shared by every clone, so no two
    /// fenced operations of this term ever carry the same rank, and every rank
    /// of term `b + 1` dominates every rank of term `b`.
    ///
    /// The first rank a new leader mints must be installed as a fence with
    /// `RankedRegister::read` before any fenced work: winning a term does not
    /// by itself stop the previous leader's writes.
    ///
    /// # Errors
    ///
    /// - [`LeaderElectionError::LeaseLost`] if the term is gone.
    /// - [`LeaderElectionError::RankExhausted`] before the sequence would wrap,
    ///   which would let a stale rank compare as fresh.
    #[cfg(feature = "recipes-ranked-register")]
    #[cfg_attr(feature = "trace", tracing::instrument(level = "trace", skip_all, err))]
    pub fn next_rank(&self) -> Result<Rank> {
        self.check()?;
        let sequence = self
            .state
            .sequence
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(1)
            })
            .map_err(|_| LeaderElectionError::RankExhausted)?;
        Ok(self.rank_at(sequence))
    }

    /// A fencing rank of this term at an explicit sequence
    ///
    /// For callers keeping their own sequence. Mixing this with
    /// [`next_rank`](Self::next_rank) can mint the same rank twice.
    ///
    /// # Errors
    ///
    /// [`LeaderElectionError::LeaseLost`] if the term is gone.
    #[cfg(feature = "recipes-ranked-register")]
    #[cfg_attr(feature = "trace", tracing::instrument(level = "trace", skip_all, err))]
    pub fn rank(&self, sequence: u32) -> Result<Rank> {
        self.check()?;
        Ok(self.rank_at(sequence))
    }

    /// The ballot goes in the high half and the sequence in the low half, which
    /// is what orders every rank of a term above every rank of its predecessor.
    #[cfg(feature = "recipes-ranked-register")]
    fn rank_at(&self, sequence: u32) -> Rank {
        Rank::new(sequence, self.state.ballot as u32)
    }
}

// ============================================================================
// OUTCOME
// ============================================================================

/// How a [`LeaderElector::lead`] call ended
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeadOutcome<T> {
    /// The work returned a value
    ///
    /// The term was not observed to end before the work finished, which is not
    /// the same as the work having held it throughout: only the checks on
    /// [`LeaseHandle`] establish that, at the moments the work made them.
    Completed {
        /// What the work returned
        value: T,
        /// Whether the term was handed back cleanly
        ///
        /// A clean handover lets the successor claim immediately; a failed one
        /// only costs it one lease of waiting, which is why the resign is
        /// best-effort and reported rather than returned as an error.
        released: bool,
    },
    /// The term ended before the work did, and the work future was dropped
    ///
    /// Either a renewal found the record taken, or the belief horizon passed.
    /// A claim that only committed after its own lease had run out reports this
    /// too, without ever starting the work.
    /// Anything the work started that outlives cancellation must be fenced by
    /// the ballot: dropping a future stops the code, not its side effects.
    LeaseLost,
}

// ============================================================================
// ELECTOR
// ============================================================================

/// Campaigns for a term, holds it while the caller works, gives it back
///
/// The lease is maintained in the same task as the work, never in a detached
/// one: if the work stops being polled, renewals stop too, and the term expires
/// instead of being held by a process that is no longer running.
///
/// # Thread safety
///
/// [`Clone`], [`Send`] and [`Sync`]. Two electors sharing a `leader_id` on one
/// subspace are two contenders like any other, not one leader.
#[derive(Clone)]
pub struct LeaderElector {
    db: Arc<Database>,
    election: LeaderElection,
    leader_id: String,
    config: ElectorConfig,
    clock: Arc<dyn Clock>,
    timer: Arc<dyn Timer>,
    /// Resolved at construction, so an elector jitters the same way for as long
    /// as it lives whether or not the config named a schedule.
    jitter: JitterSchedule,
    /// Resolved at construction, same reason.
    token_source: TokenSource,
}

impl fmt::Debug for LeaderElector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LeaderElector")
            .field("election", &self.election)
            .field("leader_id", &self.leader_id)
            .field("config", &self.config)
            .field("clock", &self.clock)
            .field("timer", &self.timer)
            .field("jitter", &self.jitter)
            .finish_non_exhaustive()
    }
}

/// Sixteen bytes of token from the environment's generator.
///
/// The all-zero token is the vacancy sentinel and is refused by the protocol,
/// so a draw that lands on it is nudged off rather than redrawn: redrawing
/// would consume a variable number of values and make the run depend on how
/// often it happened.
fn rng_token_source(rng: Arc<dyn Rng>) -> TokenSource {
    TokenSource::new(move || {
        let mut bytes = [0u8; 16];
        bytes[..8].copy_from_slice(&rng.next_u64().to_be_bytes());
        bytes[8..].copy_from_slice(&rng.next_u64().to_be_bytes());
        if bytes == [0u8; 16] {
            bytes[0] = 1;
        }
        ClaimToken::from_bytes(bytes)
    })
}

impl LeaderElector {
    /// Build an elector over `subspace`
    ///
    /// `leader_id` identifies the process, not the term: it is what the record
    /// advertises and what an operator reads. Reusing it after a restart is
    /// harmless, because a restarted process inherits nothing (its old term
    /// carries a token it no longer has).
    ///
    /// `env` supplies the two effects this layer must not reach for: the clock
    /// every attempt, grant and handle measures against, and the generator the
    /// campaign jitter and claim tokens come from when the config does not name
    /// its own. [`Environment::default`] is the production choice;
    /// [`Environment::with_seed`] makes the whole campaign replay. `timer` is
    /// how the elector waits, which [`Environment`] deliberately does not
    /// provide.
    ///
    /// # Errors
    ///
    /// [`LeaderElectionError::InvalidArgument`] if `leader_id` is empty or
    /// longer than [`MAX_LEADER_ID_LEN`], or if the configured lease exceeds
    /// the election's advertised maximum.
    pub fn new(
        db: Arc<Database>,
        subspace: Subspace,
        leader_id: impl Into<String>,
        config: ElectorConfig,
        env: Environment,
        timer: Arc<dyn Timer>,
    ) -> Result<Self> {
        let leader_id = leader_id.into();
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
        let jitter = config.jitter.unwrap_or_else(|| {
            JitterSchedule::new(env.rng().next_u64(), config.renew_interval() / 4)
        });
        let token_source = config
            .token_source
            .clone()
            .unwrap_or_else(|| rng_token_source(Arc::clone(env.rng())));
        let elector = Self {
            db,
            election: LeaderElection::new(subspace),
            leader_id,
            config,
            clock: Arc::clone(env.clock()),
            timer,
            jitter,
            token_source,
        };
        elector.check_lease_fits()
    }

    /// Set the ceiling on advertised leases, as
    /// [`LeaderElection::with_max_advertised_lease`] does
    ///
    /// # Errors
    ///
    /// [`LeaderElectionError::InvalidConfig`] if the ceiling is zero or below
    /// this elector's own lease.
    pub fn with_max_advertised_lease(mut self, max: Duration) -> Result<Self> {
        self.election = self.election.with_max_advertised_lease(max)?;
        self.check_lease_fits()
    }

    /// The primitives this elector drives, for composition: reading the
    /// history, watching the keys, or running a step in a caller's own
    /// transaction.
    pub fn election(&self) -> &LeaderElection {
        &self.election
    }

    /// The identifier this elector claims under
    pub fn leader_id(&self) -> &str {
        &self.leader_id
    }

    /// The lease schedule this elector runs on
    pub fn config(&self) -> &ElectorConfig {
        &self.config
    }

    /// The clock every derived attempt, grant and handle measures against
    ///
    /// Only [`Clock::monotonic`] is ever read: everything this layer computes
    /// is an elapsed duration on this one instance.
    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    /// The timer this elector waits on
    pub fn timer(&self) -> &Arc<dyn Timer> {
        &self.timer
    }

    /// The campaign jitter schedule in force, whether it came from the config
    /// or from the environment
    pub fn jitter(&self) -> JitterSchedule {
        self.jitter
    }

    // ========================================================================
    // LEADING
    // ========================================================================

    /// Campaign for the term, then run `work` while holding it
    ///
    /// Returns when the work finishes ([`LeadOutcome::Completed`]) or when the
    /// term ends first ([`LeadOutcome::LeaseLost`]), whichever comes first. The
    /// campaign has no timeout: a contender that never wins waits forever, so
    /// give up by dropping the future.
    ///
    /// # Lifecycle
    ///
    /// | Event | What happens |
    /// |---|---|
    /// | the claim commits too late to be usable | the work is never started, [`LeadOutcome::LeaseLost`] |
    /// | work returns | the handle is marked lost first, then a bounded best-effort resign, then [`LeadOutcome::Completed`] |
    /// | renewal finds the record taken | the handle is marked lost, the work future is dropped, [`LeadOutcome::LeaseLost`] |
    /// | belief horizon passes | same, without waiting for any renewal in flight |
    /// | this future is dropped | an ungraceful release: no resign, the successor waits out one lease |
    ///
    /// # Cancellation
    ///
    /// Losing the term drops the work future. Dropping a future stops the code
    /// at its next await point; it does not undo what already happened, and it
    /// cannot stop a task the work spawned elsewhere. Work with effects outside
    /// this process must fence them with [`LeaseHandle::ballot`], and make its
    /// progress durable before announcing it, so a successor can redrive
    /// whatever was left half-done.
    ///
    /// # Errors
    ///
    /// Any [`LeaderElectionError`] the campaign hits, including
    /// [`LeaderElectionError::CorruptRecord`] on a record this build does not
    /// understand.
    #[cfg_attr(
        feature = "trace",
        tracing::instrument(
            level = "debug",
            skip_all,
            fields(leader_id = %self.leader_id),
            err
        )
    )]
    pub async fn lead<W, Fut, T>(&self, work: W) -> Result<LeadOutcome<T>>
    where
        W: FnOnce(LeaseHandle) -> Fut,
        Fut: Future<Output = T>,
    {
        let grant = self.campaign().await?;
        let horizon = self.horizon_of(&grant);

        // A grant is anchored before its write is issued, so a claim that took
        // longer to commit than the lease it was asking for comes back already
        // expired. It is a real term in the database, but not one this process
        // may ever act on: start nothing rather than start the work and cancel
        // it at the first poll.
        if !refresh_still_applies(self.clock.monotonic(), horizon) {
            #[cfg(feature = "trace")]
            tracing::warn!(
                ballot = grant.ballot(),
                "the claim outlived its own lease, the term is unusable"
            );
            return Ok(LeadOutcome::LeaseLost);
        }

        #[cfg(feature = "trace")]
        tracing::info!(
            ballot = grant.ballot(),
            horizon_ms = horizon.as_millis() as u64,
            "term acquired, starting work"
        );

        let state = Arc::new(LeaseState::new(
            grant.ballot(),
            horizon,
            Arc::clone(&self.clock),
        ));
        // The renewal loop replaces the grant on every applied renewal, and the
        // resign at the end needs the last one, so it lives outside both.
        let held = Mutex::new(grant);

        let work = work(LeaseHandle {
            state: Arc::clone(&state),
        });
        futures::pin_mut!(work);
        let driver = self.drive_lease(&state, &held);
        futures::pin_mut!(driver);

        match futures::future::select(work, driver).await {
            Either::Left((value, _driver)) => {
                // Ordering matters: every clone of the handle is invalidated
                // before the term is offered to anyone else. A resign that
                // landed first would let a successor start while this process
                // still believed it led.
                state.mark_lost();
                let grant = self.held_grant(&held);
                let released = self.release(&grant).await;
                Ok(LeadOutcome::Completed { value, released })
            }
            Either::Right(((), _work)) => {
                state.mark_lost();
                // `_work` is a pinned reference; the future itself is dropped
                // when this frame returns, which is immediately.
                Ok(LeadOutcome::LeaseLost)
            }
        }
    }

    // ========================================================================
    // FOLLOWERS
    // ========================================================================

    /// Read whatever occupies the leader key
    ///
    /// Named for what it can promise. One read establishes what the record
    /// says, never that its holder is alive: a crashed leader stays "current"
    /// until somebody waits out its lease and steals the term.
    ///
    /// # Errors
    ///
    /// [`LeaderElectionError::CorruptRecord`] on a record this build does not
    /// understand, or any error the read hit.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip_all, err))]
    pub async fn current_record(&self) -> Result<Option<LeaderRecord>> {
        self.db
            .run(|txn, _| async move { self.election.leader(&txn).await })
            .await
    }

    /// Wait for leadership itself to change
    ///
    /// Parks on the term key, which moves only on a claim, a steal or a resign,
    /// so renewals do not wake followers. The wake-up is a hint: watches
    /// coalesce, and a term that flaps back to its previous holder may not
    /// produce one at all. Always re-read.
    ///
    /// # Errors
    ///
    /// Any error hit while arming the watch. Errors from the watch itself are
    /// treated as wake-ups, since re-reading is the answer either way.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip_all, err))]
    pub async fn wait_for_term_change(&self) -> Result<()> {
        let watch = self.arm_term_watch().await?;
        let _ = watch.await;
        Ok(())
    }

    // ========================================================================
    // INTERNALS
    // ========================================================================

    fn check_lease_fits(self) -> Result<Self> {
        if self.config.lease.as_duration() > self.election.max_advertised_lease() {
            return Err(LeaderElectionError::InvalidConfig(format!(
                "lease {} exceeds the advertised maximum {:?}",
                self.config.lease,
                self.election.max_advertised_lease()
            )));
        }
        Ok(self)
    }

    fn horizon_of(&self, grant: &LeaseGrant) -> Duration {
        belief_horizon(
            grant.acquired_at(),
            grant.lease().as_duration(),
            self.config.safety_margin,
        )
    }

    fn held_grant(&self, held: &Mutex<LeaseGrant>) -> LeaseGrant {
        held.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Claim, and keep claiming, until the term is ours.
    ///
    /// The attempt is created outside the transaction so that the lease is
    /// anchored before the write is issued and so a retry after an unknown
    /// commit can recognize its own record. A superseded attempt is spent: the
    /// next round starts a fresh one with a fresh token.
    async fn campaign(&self) -> Result<LeaseGrant> {
        // Threaded across transactions, and across `db.run` retries within
        // one: how long this process has watched the record hold still is the
        // only thing that ever authorizes a steal.
        let observation = Mutex::new(LeaseObservation::new());
        let mut round: u64 = 0;

        loop {
            let attempt = ClaimAttempt::new(self.token_source.issue(), self.clock.monotonic())?;

            let (outcome, watch) = self
                .db
                .run(|txn, _| {
                    let attempt = &attempt;
                    let observation = &observation;
                    async move {
                        let seen = *lock(observation);
                        let (outcome, updated) = self
                            .election
                            .try_claim(
                                &txn,
                                &self.leader_id,
                                self.config.lease,
                                attempt,
                                seen,
                                || self.clock.monotonic(),
                            )
                            .await?;
                        *lock(observation) = updated;

                        // Armed in the transaction that read the record, so
                        // nothing can slip between the read and the watch. It
                        // is awaited only once `run` has committed.
                        let watch = match outcome {
                            ClaimOutcome::Denied { .. } => Some(self.election.watch_term(&txn)),
                            _ => None,
                        };
                        Ok::<_, LeaderElectionError>((outcome, watch))
                    }
                })
                .await?;

            let backoff = match outcome {
                ClaimOutcome::Won(grant) => return Ok(grant),
                ClaimOutcome::Denied { retry_after, .. } => {
                    #[cfg(feature = "trace")]
                    tracing::debug!(
                        round,
                        retry_after_ms = retry_after.as_millis() as u64,
                        "denied, parking until the record could be stale"
                    );
                    retry_after.min(self.config.backoff_cap)
                }
                ClaimOutcome::Superseded => {
                    #[cfg(feature = "trace")]
                    tracing::warn!(round, "attempt superseded, campaigning with a fresh token");
                    Duration::ZERO
                }
            };

            let delay = backoff.saturating_add(self.jitter.jitter_for(round));
            round = round.wrapping_add(1);

            let sleep = self.timer.sleep(delay);
            match watch {
                // Whichever fires first, the answer is the same: read again.
                Some(watch) => {
                    let _ = futures::future::select(sleep, watch).await;
                }
                None => sleep.await,
            }
        }
    }

    /// Renew until the term ends.
    ///
    /// Returns only when leadership is over: a renewal found the record taken,
    /// or the belief horizon passed. Both are terminal, and both mark the
    /// handle before returning so the work sees it at its next check even
    /// though it is about to be dropped.
    async fn drive_lease(&self, state: &LeaseState, held: &Mutex<LeaseGrant>) {
        loop {
            let grant = self.held_grant(held);
            let horizon = self.horizon_of(&grant);

            // Config validation guarantees the renewal comes due strictly
            // before the horizon, so this wait can never overshoot it.
            let due = grant
                .acquired_at()
                .saturating_add(self.config.renew_interval);
            let now = self.clock.monotonic();
            if due > now {
                self.timer.sleep(due - now).await;
            }

            let now = self.clock.monotonic();
            if now >= horizon {
                #[cfg(feature = "trace")]
                tracing::warn!(
                    ballot = grant.ballot(),
                    "belief horizon reached, stopping leadership"
                );
                state.mark_lost();
                return;
            }

            // A renewal never gets to spend time the leader no longer has: its
            // budget is capped at whatever is left before the horizon.
            let budget = self.config.refresh_timeout.min(horizon - now);
            let attempt = RefreshAttempt::new(&grant, now);
            let refresh = self.refresh_once(&grant, &attempt);
            futures::pin_mut!(refresh);

            let outcome = match futures::future::select(refresh, self.timer.sleep(budget)).await {
                Either::Left((outcome, _timeout)) => outcome,
                Either::Right(((), _refresh)) => {
                    // The transaction is abandoned by dropping it. It may still
                    // commit, which is harmless: the next renewal reads the
                    // record and recognizes its own generation bump.
                    state.set_status(STATUS_JEOPARDY);
                    #[cfg(feature = "trace")]
                    tracing::warn!(
                        ballot = grant.ballot(),
                        budget_ms = budget.as_millis() as u64,
                        "renewal timed out, in jeopardy"
                    );
                    continue;
                }
            };

            // The horizon wins over any result that arrives after it. A renewal
            // in flight while the belief expired is discarded, because a
            // contender may already have started counting. This covers the
            // recovered grant too: `refresh` reports what the database says and
            // takes no clock, and a renewal recovered from an unknown commit
            // keeps its original pre-issuance anchor, so a recovery that took
            // longer than the lease hands back a grant that expired on the way.
            // The horizon checked here is the one in force before this renewal,
            // which is earlier than the renewed grant's own, so believing the
            // result requires having learned of it while the previous belief
            // still stood.
            if !refresh_still_applies(self.clock.monotonic(), horizon) {
                #[cfg(feature = "trace")]
                tracing::warn!(
                    ballot = grant.ballot(),
                    "renewal landed after the horizon, discarded"
                );
                state.mark_lost();
                return;
            }

            match outcome {
                Ok(RefreshOutcome::Refreshed(renewed)) => {
                    state.set_horizon(self.horizon_of(&renewed));
                    state.set_status(STATUS_LEADING);
                    *held.lock().unwrap_or_else(PoisonError::into_inner) = renewed;
                }
                Ok(RefreshOutcome::Lost { observed }) => {
                    #[cfg(feature = "trace")]
                    tracing::warn!(
                        ballot = grant.ballot(),
                        observed_ballot = observed.as_ref().map(LeaderRecord::ballot),
                        "term taken over, stopping leadership"
                    );
                    let _ = observed;
                    state.mark_lost();
                    return;
                }
                Err(_error) => {
                    // The database is unreachable, not necessarily lost: keep
                    // believing until the horizon, and keep trying.
                    state.set_status(STATUS_JEOPARDY);
                    #[cfg(feature = "trace")]
                    tracing::warn!(
                        ballot = grant.ballot(),
                        error = %_error,
                        "renewal failed, in jeopardy"
                    );
                    let now = self.clock.monotonic();
                    let backoff = self
                        .config
                        .jeopardy_backoff
                        .min(horizon.saturating_sub(now));
                    self.timer.sleep(backoff).await;
                }
            }
        }
    }

    async fn refresh_once(
        &self,
        grant: &LeaseGrant,
        attempt: &RefreshAttempt,
    ) -> Result<RefreshOutcome> {
        self.db
            .run(|txn, _| async move { self.election.refresh(&txn, grant, attempt).await })
            .await
    }

    /// Hand the term back, if it can be done quickly.
    ///
    /// Bounded because the work is already finished: waiting on an unreachable
    /// database to say goodbye helps nobody, and the only cost of skipping it
    /// is that the successor waits out one lease.
    async fn release(&self, grant: &LeaseGrant) -> bool {
        let resign = self
            .db
            .run(|txn, _| async move { self.election.resign(&txn, grant).await });
        futures::pin_mut!(resign);

        match futures::future::select(resign, self.timer.sleep(self.config.resign_timeout)).await {
            Either::Left((Ok(ResignOutcome::Resigned), _)) => {
                #[cfg(feature = "trace")]
                tracing::info!(ballot = grant.ballot(), "term handed back");
                true
            }
            Either::Left((Ok(ResignOutcome::NotHolder), _)) => {
                #[cfg(feature = "trace")]
                tracing::warn!(
                    ballot = grant.ballot(),
                    "resign found the term already gone"
                );
                false
            }
            Either::Left((Err(_error), _)) => {
                #[cfg(feature = "trace")]
                tracing::warn!(
                    ballot = grant.ballot(),
                    error = %_error,
                    "resign failed, the successor will wait out the lease"
                );
                false
            }
            Either::Right(((), _resign)) => {
                #[cfg(feature = "trace")]
                tracing::warn!(
                    ballot = grant.ballot(),
                    "resign timed out, the successor will wait out the lease"
                );
                false
            }
        }
    }

    async fn arm_term_watch(&self) -> Result<BoxFuture<'static, FdbResult<()>>> {
        self.db
            .run(|txn, _| async move { Ok(self.election.watch_term(&txn)) })
            .await
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// A clock a test drives by hand: it only moves when the test says so.
    #[derive(Debug, Default)]
    struct MockClock {
        nanos: AtomicU64,
    }

    impl MockClock {
        fn advance(&self, by: Duration) {
            self.nanos.fetch_add(nanos_of(by), Ordering::SeqCst);
        }
    }

    impl Clock for MockClock {
        fn monotonic(&self) -> Duration {
            Duration::from_nanos(self.nanos.load(Ordering::SeqCst))
        }

        /// Never read by this layer, and a test that starts branching on it is
        /// testing the wrong thing.
        fn wall(&self) -> Duration {
            unreachable!("the handle layer must never read a wall clock")
        }
    }

    /// A timer that never actually waits.
    #[derive(Debug, Default)]
    struct MockTimer;

    impl Timer for MockTimer {
        fn sleep(&self, _duration: Duration) -> BoxFuture<'static, ()> {
            Box::pin(futures::future::ready(()))
        }
    }

    static_assertions::assert_impl_all!(LeaseHandle: Send, Sync);
    static_assertions::assert_impl_all!(LeaderElector: Send, Sync);

    /// Leading is normally spawned as a task, so the future it returns has to
    /// be `Send`. Never called: type-checking it is the whole point.
    #[allow(dead_code)]
    fn the_lead_future_can_be_spawned(elector: &LeaderElector) {
        fn assert_send<T: Send>(_: &T) {}
        assert_send(&elector.lead(|handle| async move { handle.ballot() }));
    }

    /// Every effect an elector needs comes in through the constructor. Never
    /// called either: building one needs a database, and the signature is what
    /// this pins down.
    #[allow(dead_code)]
    fn an_elector_takes_its_effects_as_dependencies(db: Arc<Database>) -> Result<LeaderElector> {
        let env = Environment::new(
            Arc::new(MockClock::default()),
            Arc::new(crate::env::SeededRng::new(7)),
        );
        LeaderElector::new(
            db,
            Subspace::all(),
            "worker-1",
            ElectorConfig::new(Duration::from_secs(10))?,
            env,
            Arc::new(MockTimer),
        )
    }

    fn handle_at(ballot: u64, horizon: Duration, clock: Arc<dyn Clock>) -> LeaseHandle {
        LeaseHandle {
            state: Arc::new(LeaseState::new(ballot, horizon, clock)),
        }
    }

    // ---- config ----------------------------------------------------------

    #[test]
    fn defaults_are_derived_from_the_lease() {
        let config = ElectorConfig::new(Duration::from_secs(9)).unwrap();

        assert_eq!(config.renew_interval(), Duration::from_secs(3));
        assert_eq!(config.max_clock_rate_error(), DEFAULT_MAX_CLOCK_RATE_ERROR);
        assert_eq!(config.backoff_cap(), Duration::from_secs(9));
        assert!(config.renew_interval() + config.safety_margin() < Duration::from_secs(9));
    }

    #[test]
    fn the_margin_follows_the_clock_rate_formula() {
        let lease = Duration::from_secs(10);
        let error = 1e-3;
        let config = ElectorConfig::new(lease)
            .unwrap()
            .with_scheduling_allowance(Duration::ZERO)
            .unwrap()
            .with_max_clock_rate_error(error)
            .unwrap();

        let expected = lease.mul_f64(2.0 * error / (1.0 + error));
        assert_eq!(config.safety_margin(), expected);

        // The allowance is additive on top of the rate term, not part of it.
        let with_allowance = config
            .clone()
            .with_scheduling_allowance(Duration::from_millis(50))
            .unwrap();
        assert_eq!(
            with_allowance.safety_margin(),
            expected + Duration::from_millis(50)
        );
    }

    #[test]
    fn a_slow_leader_stops_before_a_fast_contender_can_steal() {
        let lease = Duration::from_secs(10);

        for error in [0.0, 1e-6, 1e-3, 1e-2, 0.1] {
            // Zero scheduling allowance: this is the rate term alone, at the
            // exact bound the contract assumes.
            let config = ElectorConfig::new(lease)
                .unwrap()
                .with_scheduling_allowance(Duration::ZERO)
                .unwrap()
                .with_max_clock_rate_error(error)
                .unwrap();
            let margin = config.safety_margin();

            // The leader's clock is as slow as the bound allows, so the
            // duration it measures as `lease - margin` takes longer in real
            // time.
            let leader_stops_at = (lease - margin).as_secs_f64() * (1.0 + error);
            // The contender's clock is as fast as the bound allows, and it
            // cannot have started timing before the leader's write committed
            // (taken as the origin here, which is the earliest possible).
            let contender_steals_at = lease.as_secs_f64() / (1.0 + error);

            assert!(
                leader_stops_at <= contender_steals_at,
                "rate error {error}: leader believes until {leader_stops_at}s but a contender \
                 could steal at {contender_steals_at}s",
            );
        }
    }

    #[test]
    fn a_renewal_that_cannot_run_twice_per_lease_is_rejected() {
        let error = ElectorConfig::new(Duration::from_secs(10))
            .unwrap()
            .with_renew_interval(Duration::from_secs(6))
            .unwrap_err();

        assert!(
            matches!(error, LeaderElectionError::InvalidConfig(_)),
            "got {error:?}"
        );
    }

    #[test]
    fn a_margin_that_crowds_the_renewal_is_rejected() {
        let error = ElectorConfig::new(Duration::from_secs(10))
            .unwrap()
            .with_scheduling_allowance(Duration::from_secs(7))
            .unwrap_err();

        assert!(
            matches!(error, LeaderElectionError::InvalidConfig(_)),
            "got {error:?}"
        );
    }

    #[test]
    fn an_impossible_clock_rate_bound_is_rejected() {
        let config = ElectorConfig::new(Duration::from_secs(10)).unwrap();

        for bad in [-1e-3, 1.0, 2.0, f64::NAN, f64::INFINITY] {
            let error = config.clone().with_max_clock_rate_error(bad).unwrap_err();
            assert!(
                matches!(error, LeaderElectionError::InvalidConfig(_)),
                "rate {bad} was accepted",
            );
        }
    }

    #[test]
    fn a_zero_lease_is_rejected() {
        let error = ElectorConfig::new(Duration::ZERO).unwrap_err();
        assert!(
            matches!(error, LeaderElectionError::InvalidArgument(_)),
            "got {error:?}"
        );
    }

    #[test]
    fn jitter_is_reproducible_and_bounded() {
        let window = Duration::from_millis(400);
        let schedule = JitterSchedule::new(0xDEAD_BEEF, window);

        let first: Vec<_> = (0..64).map(|round| schedule.jitter_for(round)).collect();
        let again: Vec<_> = (0..64).map(|round| schedule.jitter_for(round)).collect();
        assert_eq!(first, again, "the schedule must replay identically");
        assert!(first.iter().all(|delay| *delay < window));
        // Two different seeds must not march in lockstep.
        let other = JitterSchedule::new(1, window);
        assert_ne!(first[0], other.jitter_for(0));
        assert_eq!(
            JitterSchedule::new(7, Duration::ZERO).jitter_for(3),
            Duration::ZERO
        );
    }

    #[test]
    fn a_fresh_config_leaves_both_hooks_to_the_environment() {
        let config = ElectorConfig::new(Duration::from_secs(9)).unwrap();

        assert!(config.jitter().is_none());
        assert!(config.token_source().is_none());

        let pinned = JitterSchedule::new(1, Duration::from_millis(10));
        let config = config.with_jitter(pinned);
        assert_eq!(config.jitter(), Some(pinned));
    }

    // ---- tokens ----------------------------------------------------------

    #[test]
    fn tokens_from_the_environment_replay_and_advance() {
        let source = rng_token_source(Arc::new(crate::env::SeededRng::new(11)));
        let replay = rng_token_source(Arc::new(crate::env::SeededRng::new(11)));

        let issued: Vec<_> = (0..8).map(|_| source.issue()).collect();
        let again: Vec<_> = (0..8).map(|_| replay.issue()).collect();

        assert_eq!(issued, again, "the same seed must replay the same tokens");
        assert!(
            issued.windows(2).all(|pair| pair[0] != pair[1]),
            "a term must not reuse the token of the one before: {issued:?}"
        );
        assert!(issued.iter().all(|token| !token.is_zero()));
    }

    #[test]
    fn a_token_drawn_all_zero_is_nudged_off_the_sentinel() {
        /// The one draw the vacancy sentinel forbids.
        #[derive(Debug)]
        struct ZeroRng;

        impl crate::env::Rng for ZeroRng {
            fn next_u64(&self) -> u64 {
                0
            }
        }

        let token = rng_token_source(Arc::new(ZeroRng)).issue();

        assert!(!token.is_zero(), "the sentinel is not a claim");
        assert_eq!(token.as_bytes()[0], 1);
    }

    // ---- horizon ---------------------------------------------------------

    #[test]
    fn the_horizon_gives_up_the_margin_and_saturates() {
        assert_eq!(
            belief_horizon(
                Duration::from_secs(100),
                Duration::from_secs(10),
                Duration::from_secs(1)
            ),
            Duration::from_secs(109)
        );
        // A margin larger than the lease means no belief at all, not a panic.
        assert_eq!(
            belief_horizon(
                Duration::from_secs(5),
                Duration::from_secs(1),
                Duration::from_secs(10)
            ),
            Duration::from_secs(5)
        );
    }

    #[test]
    fn a_renewal_landing_on_the_horizon_is_too_late() {
        let horizon = Duration::from_secs(10);

        assert!(refresh_still_applies(
            horizon - Duration::from_nanos(1),
            horizon
        ));
        // Exactly on the boundary: the contender's window may have opened, so
        // the result is discarded.
        assert!(!refresh_still_applies(horizon, horizon));
        assert!(!refresh_still_applies(
            horizon + Duration::from_nanos(1),
            horizon
        ));
    }

    #[test]
    fn a_grant_recovered_after_its_own_anchor_expired_is_rejected() {
        // A renewal issued at 10s against a 3s lease keeps that anchor however
        // long its transaction takes, recovery retries included: the grant it
        // hands back is anchored at 10s, not at reply time. The leading loop
        // resamples the clock before applying anything, so a reply that only
        // arrives at 13s is refused rather than believed.
        let anchor = Duration::from_secs(10);
        let horizon = belief_horizon(anchor, Duration::from_secs(3), Duration::from_millis(100));
        assert_eq!(horizon, Duration::from_millis(12_900));

        assert!(refresh_still_applies(Duration::from_secs(12), horizon));
        assert!(!refresh_still_applies(Duration::from_secs(13), horizon));

        // The same arithmetic is what refuses a claim whose commit outlived the
        // lease it was asking for, before any work is started.
        assert!(!refresh_still_applies(
            anchor + Duration::from_secs(3),
            horizon
        ));
    }

    // ---- handle ----------------------------------------------------------

    #[test]
    fn a_handle_goes_stale_on_its_own_clock() {
        let clock = Arc::new(MockClock::default());
        let handle = handle_at(4, Duration::from_secs(10), clock.clone());
        let clone = handle.clone();

        assert_eq!(handle.status(), LeaseStatus::Leading);
        assert!(handle.check().is_ok());

        // Nothing tells the handle the term ended: the elector could have been
        // dropped, the task never polled again. The clock is enough.
        clock.advance(Duration::from_secs(10));

        assert_eq!(handle.status(), LeaseStatus::Lost);
        assert_eq!(clone.status(), LeaseStatus::Lost);
        assert_eq!(handle.check().unwrap_err(), LeaseLostError { ballot: 4 });
    }

    #[test]
    fn jeopardy_still_counts_as_holding_the_term() {
        let clock = Arc::new(MockClock::default());
        let handle = handle_at(9, Duration::from_secs(10), clock.clone());

        handle.state.set_status(STATUS_JEOPARDY);
        assert_eq!(handle.status(), LeaseStatus::Jeopardy);
        assert!(handle.check().is_ok(), "jeopardy is not a lost lease");

        // A renewal that gets through before the horizon takes it back.
        handle.state.set_status(STATUS_LEADING);
        assert_eq!(handle.status(), LeaseStatus::Leading);
    }

    #[test]
    fn losing_the_term_is_terminal() {
        let clock = Arc::new(MockClock::default());
        let handle = handle_at(2, Duration::from_secs(10), clock);

        handle.state.mark_lost();
        // A late renewal must not resurrect a handle the caller was already
        // told to stop trusting.
        handle.state.set_status(STATUS_LEADING);
        handle.state.set_horizon(Duration::from_secs(1_000));

        assert_eq!(handle.status(), LeaseStatus::Lost);
    }

    // ---- fencing ---------------------------------------------------------

    #[cfg(feature = "recipes-ranked-register")]
    #[test]
    fn every_rank_of_a_term_dominates_the_one_before() {
        let clock = Arc::new(MockClock::default());
        let horizon = Duration::from_secs(10);

        let older = handle_at(7, horizon, clock.clone());
        let newer = handle_at(8, horizon, clock);

        let highest_old = (0..1_000)
            .map(|_| older.next_rank().unwrap())
            .max()
            .unwrap();
        let lowest_new = newer.next_rank().unwrap();

        assert!(
            highest_old < lowest_new,
            "term 7 rank {highest_old:?} is not below term 8 rank {lowest_new:?}",
        );
    }

    #[cfg(feature = "recipes-ranked-register")]
    #[test]
    fn clones_mint_ranks_from_one_counter() {
        let clock = Arc::new(MockClock::default());
        let handle = handle_at(3, Duration::from_secs(10), clock);
        let clone = handle.clone();

        let ranks = [
            handle.next_rank().unwrap(),
            clone.next_rank().unwrap(),
            handle.next_rank().unwrap(),
        ];

        assert_eq!(ranks[0].sequence(), 3, "the ballot lives in the high half");
        assert!(ranks[0] < ranks[1] && ranks[1] < ranks[2]);
    }

    #[cfg(feature = "recipes-ranked-register")]
    #[test]
    fn the_sequence_is_refused_before_it_wraps() {
        let clock = Arc::new(MockClock::default());
        let handle = handle_at(1, Duration::from_secs(10), clock);
        handle.state.sequence.store(u32::MAX, Ordering::SeqCst);

        let error = handle.next_rank().unwrap_err();
        assert!(
            matches!(error, LeaderElectionError::RankExhausted),
            "got {error:?}"
        );
        // A wrapped sequence would compare as fresh, so the refusal is sticky.
        assert!(matches!(
            handle.next_rank().unwrap_err(),
            LeaderElectionError::RankExhausted
        ));
    }

    #[cfg(feature = "recipes-ranked-register")]
    #[test]
    fn a_stale_handle_mints_no_ranks() {
        let clock = Arc::new(MockClock::default());
        let handle = handle_at(5, Duration::from_secs(10), clock.clone());

        clock.advance(Duration::from_secs(10));

        assert!(matches!(
            handle.next_rank().unwrap_err(),
            LeaderElectionError::LeaseLost(LeaseLostError { ballot: 5 })
        ));
        assert!(matches!(
            handle.rank(0).unwrap_err(),
            LeaderElectionError::LeaseLost(_)
        ));
    }
}
