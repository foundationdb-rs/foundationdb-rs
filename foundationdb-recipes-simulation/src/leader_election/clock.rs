//! The skewed clock each client measures time with.
//!
//! The recipe reads time through the [`Clock`] of the [`Environment`] it was
//! handed, and never from the machine. That is its trust boundary, and this
//! module is what sits on the other side of it: [`SkewedClock`] is a `Clock`
//! that decorates the simulator's own, so each client gets its own distorted
//! view of time and that view is the *only* one the recipe ever sees. True
//! simulated time, the undecorated clock underneath, stays reserved for the
//! check phase, which uses it as an oracle the participants have no access to.
//!
//! [`Environment`]: foundationdb::env::Environment
//!
//! # What may be distorted, and by how much
//!
//! Offset does not matter. Every quantity the protocol computes (how long a
//! record has held still, how much of a lease is left) is an elapsed time
//! measured by one clock, and a constant offset cancels out of a difference. It
//! is injected anyway, because a recipe that accidentally compared two clients'
//! timestamps would then break loudly instead of passing by luck.
//!
//! Rate error does not cancel, and it is the assumption the belief-exclusion
//! argument actually rests on: clients' clocks run within a relative error `e`
//! of true time. [`SkewMode::max_rate_error`] is that `e`, and the check phase
//! derives its tolerances from the same number.
//!
//! Jumps and regressions are injected separately from rate error, and
//! deliberately kept inside the same budget: a step of more than about
//! `e * lease` is a fault the design makes no claim about, and injecting one
//! would produce a "failure" that says nothing except that the sim broke its
//! own assumptions. What the injection is for is the code paths a step
//! exercises: a regression makes the recipe's saturating elapsed-time
//! arithmetic run, and a jump makes a leader hit its horizon early.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use foundationdb::env::Clock;

/// How much the clients' clocks are allowed to disagree
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SkewMode {
    /// Every client reads true simulated time
    None,
    /// Per-client offset and rate error, no steps
    Random,
    /// Offset, rate error at the bound, plus one jump and one regression
    Extreme,
}

impl SkewMode {
    /// Parse the `clockSkewMode` option
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "none" => Some(Self::None),
            "random" => Some(Self::Random),
            "extreme" => Some(Self::Extreme),
            _ => None,
        }
    }

    /// The name this mode is configured under
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Random => "random",
            Self::Extreme => "extreme",
        }
    }

    /// The worst-case relative clock rate error this mode admits
    ///
    /// The check phase turns this into its tolerances, and the driver turns it
    /// into the safety margin it subtracts from every belief horizon, so the
    /// two halves of the run agree on what the clocks are allowed to do.
    pub(crate) fn max_rate_error(self) -> f64 {
        match self {
            Self::None => 0.0,
            Self::Random => 0.01,
            Self::Extreme => 0.05,
        }
    }
}

/// One client's view of time
///
/// A [`Clock`] wrapping the clock underneath it, which under simulation is the
/// simulator's own. Readings are a function of that clock's reading alone, so a
/// run replays identically from the same seed.
#[derive(Debug)]
pub(crate) struct SkewedClock {
    /// The undistorted clock this is a view of
    inner: Arc<dyn Clock>,
    offset: Duration,
    rate: f64,
    /// Simulated time at which this clock steps forward, and by how much
    jump: Option<(Duration, Duration)>,
    /// Simulated time at which this clock steps backward, and by how much
    regression: Option<(Duration, Duration)>,
    /// Tracked through an atomic because [`Clock::monotonic`] takes `&self` and
    /// a `Clock` must be `Sync`, so a `Cell` will not do. Nothing branches on
    /// it, it is reported, so the ordering can be as weak as it gets.
    max_observed_skew: AtomicU64,
}

impl SkewedClock {
    /// Build a client's view of `inner`
    ///
    /// `rnd` is the environment's deterministic generator; `lease` scales the
    /// injected offset and steps, since a distortion only means anything
    /// relative to the interval the protocol measures.
    pub(crate) fn new(
        inner: Arc<dyn Clock>,
        mode: SkewMode,
        lease: Duration,
        test_duration: Duration,
        mut rnd: impl FnMut() -> u32,
    ) -> Self {
        let unit = |value: u32| f64::from(value) / f64::from(u32::MAX);
        let bound = mode.max_rate_error();

        let (offset, rate) = match mode {
            SkewMode::None => (Duration::ZERO, 1.0),
            _ => {
                // Half the bound each way, leaving the other half of the
                // tolerance budget for the steps below.
                let error = (unit(rnd()) * 2.0 - 1.0) * bound / 2.0;
                (lease.mul_f64(unit(rnd()) * 4.0), 1.0 + error)
            }
        };

        let (jump, regression) = match mode {
            SkewMode::Extreme => {
                let step = lease.mul_f64(bound / 2.0);
                (
                    Some((test_duration.mul_f64(0.25 + unit(rnd()) * 0.25), step)),
                    Some((test_duration.mul_f64(0.55 + unit(rnd()) * 0.25), step)),
                )
            }
            _ => (None, None),
        };

        Self {
            inner,
            offset,
            rate,
            jump,
            regression,
            max_observed_skew: AtomicU64::new(0),
        }
    }

    /// This clock's reading when the clock underneath reads `sim`
    fn reading_at(&self, sim: Duration) -> Duration {
        let mut nanos = self.offset.as_nanos() as u64 + sim.mul_f64(self.rate).as_nanos() as u64;
        if let Some((at, step)) = self.jump {
            if sim >= at {
                nanos = nanos.saturating_add(step.as_nanos() as u64);
            }
        }
        if let Some((at, step)) = self.regression {
            if sim >= at {
                nanos = nanos.saturating_sub(step.as_nanos() as u64);
            }
        }

        let sim_nanos = sim.as_nanos() as u64;
        let skew = nanos.abs_diff(sim_nanos);
        self.max_observed_skew.fetch_max(skew, Ordering::Relaxed);
        Duration::from_nanos(nanos)
    }

    /// The largest distance from true time this clock has been read at
    ///
    /// Dominated by the offset, which cancels out of every measurement the
    /// protocol makes. Reported so a trace of a failing run says how far the
    /// clocks were pushed, not so the check phase can derive anything from it.
    pub(crate) fn max_observed_skew(&self) -> Duration {
        Duration::from_nanos(self.max_observed_skew.load(Ordering::Relaxed))
    }

    /// How fast this clock runs relative to simulated time
    pub(crate) fn rate(&self) -> f64 {
        self.rate
    }
}

impl Clock for SkewedClock {
    fn monotonic(&self) -> Duration {
        self.reading_at(self.inner.monotonic())
    }

    /// The same distorted reading as [`monotonic`](Clock::monotonic).
    ///
    /// The clock underneath is the simulator's, whose wall time *is* its
    /// monotonic reading: simulated wall time counts from the UNIX epoch at
    /// simulation start. Distorting the two separately would therefore invent a
    /// disagreement the simulator cannot produce. Nothing in the recipe
    /// measures with wall time anyway, this exists to complete the trait.
    fn wall(&self) -> Duration {
        self.monotonic()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A generator whose values are fixed, so a test can say what it wants
    fn constant(value: u32) -> impl FnMut() -> u32 {
        move || value
    }

    /// A clock a test moves by hand, standing in for the simulator's
    #[derive(Debug, Default)]
    struct FixedClock(AtomicU64);

    impl FixedClock {
        fn set(&self, now: Duration) {
            self.0.store(now.as_nanos() as u64, Ordering::Relaxed);
        }
    }

    impl Clock for FixedClock {
        fn monotonic(&self) -> Duration {
            Duration::from_nanos(self.0.load(Ordering::Relaxed))
        }

        fn wall(&self) -> Duration {
            self.monotonic()
        }
    }

    /// A skewed view of a clock stopped at zero
    fn skewed(mode: SkewMode, rnd: u32) -> SkewedClock {
        SkewedClock::new(
            Arc::new(FixedClock::default()),
            mode,
            LEASE,
            RUN,
            constant(rnd),
        )
    }

    const LEASE: Duration = Duration::from_secs(10);
    const RUN: Duration = Duration::from_secs(60);

    #[test]
    fn the_unskewed_mode_reads_true_time() {
        let clock = skewed(SkewMode::None, u32::MAX / 2);
        for sim in [0u64, 1, 17, 600] {
            let sim = Duration::from_secs(sim);
            assert_eq!(clock.reading_at(sim), sim);
        }
        assert_eq!(clock.max_observed_skew(), Duration::ZERO);
    }

    #[test]
    fn the_reading_follows_the_clock_underneath() {
        // The decoration is the whole point: what the recipe reads has to be
        // this client's view of the clock the simulator advances, not a
        // timeline of its own.
        let inner = Arc::new(FixedClock::default());
        let clock = SkewedClock::new(
            inner.clone(),
            SkewMode::Random,
            LEASE,
            RUN,
            constant(u32::MAX / 3),
        );

        let mut previous = clock.monotonic();
        for secs in [1u64, 5, 30, 59] {
            let sim = Duration::from_secs(secs);
            inner.set(sim);
            assert_eq!(clock.monotonic(), clock.reading_at(sim));
            assert_eq!(clock.wall(), clock.monotonic());
            assert!(clock.monotonic() > previous, "the view must move with it");
            previous = clock.monotonic();
        }
    }

    #[test]
    fn rate_error_stays_inside_the_bound_the_check_phase_is_told_about() {
        // The check phase derives its tolerances from `max_rate_error`, so a
        // clock running outside it would make honest runs fail.
        for seed in [0u32, 1, u32::MAX / 3, u32::MAX] {
            for mode in [SkewMode::Random, SkewMode::Extreme] {
                let clock = skewed(mode, seed);
                assert!(
                    (clock.rate() - 1.0).abs() <= mode.max_rate_error(),
                    "{mode:?} produced rate {}",
                    clock.rate()
                );
            }
        }
    }

    #[test]
    fn a_regression_moves_the_reading_backwards_exactly_once() {
        let clock = skewed(SkewMode::Extreme, 0);
        let (jump_at, step) = clock.jump.expect("extreme mode injects a jump");
        let (regress_at, _) = clock.regression.expect("extreme mode injects a regression");
        assert!(jump_at < regress_at, "the steps must not coincide");

        let before = clock.reading_at(regress_at - Duration::from_millis(1));
        let after = clock.reading_at(regress_at);
        assert!(after < before, "the regression must move time backwards");
        // And it is a step, not a new rate: the gap stays the size it was.
        let later = clock.reading_at(regress_at + Duration::from_secs(5));
        assert!(later > after);
        assert!(step > Duration::ZERO);
    }

    #[test]
    fn every_injected_step_fits_in_the_tolerance_budget() {
        // A step larger than what the rate bound implies over one lease is a
        // fault the protocol makes no claim about; injecting one would fail an
        // honest run.
        let clock = skewed(SkewMode::Extreme, u32::MAX);
        let budget = LEASE.mul_f64(SkewMode::Extreme.max_rate_error());
        for step in [clock.jump, clock.regression].into_iter().flatten() {
            assert!(step.1 <= budget, "step {:?} exceeds {budget:?}", step.1);
        }
    }

    #[test]
    fn skew_modes_round_trip_through_their_names() {
        for mode in [SkewMode::None, SkewMode::Random, SkewMode::Extreme] {
            assert_eq!(SkewMode::parse(mode.as_str()), Some(mode));
        }
        assert_eq!(SkewMode::parse("wobbly"), None);
    }
}
