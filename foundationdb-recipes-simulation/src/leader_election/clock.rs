//! The skewed clock each client measures time with.
//!
//! The recipe never reads a clock of its own: every instant it works with is
//! handed to it by the caller. That is its trust boundary, and this module is
//! what sits on the other side of it. Each client gets its own distorted view
//! of time, and that view is the *only* one the recipe ever sees. True
//! simulated time stays reserved for the check phase, which uses it as an
//! oracle the participants have no access to.
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

use std::cell::Cell;
use std::time::Duration;

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
/// Readings are a function of simulated time alone, so a run replays
/// identically from the same seed.
#[derive(Debug)]
pub(crate) struct SkewedClock {
    offset: Duration,
    rate: f64,
    /// Simulated time at which this clock steps forward, and by how much
    jump: Option<(Duration, Duration)>,
    /// Simulated time at which this clock steps backward, and by how much
    regression: Option<(Duration, Duration)>,
    max_observed_skew: Cell<u64>,
}

impl SkewedClock {
    /// Build a client's clock
    ///
    /// `rnd` is the simulator's deterministic generator; `lease` scales the
    /// injected offset and steps, since a distortion only means anything
    /// relative to the interval the protocol measures.
    pub(crate) fn new(
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
            offset,
            rate,
            jump,
            regression,
            max_observed_skew: Cell::new(0),
        }
    }

    /// This clock's reading at simulated time `sim`
    pub(crate) fn now(&self, sim: Duration) -> Duration {
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
        if skew > self.max_observed_skew.get() {
            self.max_observed_skew.set(skew);
        }
        Duration::from_nanos(nanos)
    }

    /// The largest distance from true time this clock has been read at
    ///
    /// Dominated by the offset, which cancels out of every measurement the
    /// protocol makes. Reported so a trace of a failing run says how far the
    /// clocks were pushed, not so the check phase can derive anything from it.
    pub(crate) fn max_observed_skew(&self) -> Duration {
        Duration::from_nanos(self.max_observed_skew.get())
    }

    /// How fast this clock runs relative to simulated time
    pub(crate) fn rate(&self) -> f64 {
        self.rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A generator whose values are fixed, so a test can say what it wants
    fn constant(value: u32) -> impl FnMut() -> u32 {
        move || value
    }

    const LEASE: Duration = Duration::from_secs(10);
    const RUN: Duration = Duration::from_secs(60);

    #[test]
    fn the_unskewed_mode_reads_true_time() {
        let clock = SkewedClock::new(SkewMode::None, LEASE, RUN, constant(u32::MAX / 2));
        for sim in [0u64, 1, 17, 600] {
            let sim = Duration::from_secs(sim);
            assert_eq!(clock.now(sim), sim);
        }
        assert_eq!(clock.max_observed_skew(), Duration::ZERO);
    }

    #[test]
    fn rate_error_stays_inside_the_bound_the_check_phase_is_told_about() {
        // The check phase derives its tolerances from `max_rate_error`, so a
        // clock running outside it would make honest runs fail.
        for seed in [0u32, 1, u32::MAX / 3, u32::MAX] {
            for mode in [SkewMode::Random, SkewMode::Extreme] {
                let clock = SkewedClock::new(mode, LEASE, RUN, constant(seed));
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
        let clock = SkewedClock::new(SkewMode::Extreme, LEASE, RUN, constant(0));
        let (jump_at, step) = clock.jump.expect("extreme mode injects a jump");
        let (regress_at, _) = clock.regression.expect("extreme mode injects a regression");
        assert!(jump_at < regress_at, "the steps must not coincide");

        let before = clock.now(regress_at - Duration::from_millis(1));
        let after = clock.now(regress_at);
        assert!(after < before, "the regression must move time backwards");
        // And it is a step, not a new rate: the gap stays the size it was.
        let later = clock.now(regress_at + Duration::from_secs(5));
        assert!(later > after);
        assert!(step > Duration::ZERO);
    }

    #[test]
    fn every_injected_step_fits_in_the_tolerance_budget() {
        // A step larger than what the rate bound implies over one lease is a
        // fault the protocol makes no claim about; injecting one would fail an
        // honest run.
        let clock = SkewedClock::new(SkewMode::Extreme, LEASE, RUN, constant(u32::MAX));
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
