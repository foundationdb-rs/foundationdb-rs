//! The swarm-testing plan: what a run is allowed to look like.
//!
//! A simulation seed does not just pick a schedule, it picks a *configuration*.
//! This module turns a seed into one, as a pure function, so that a failing run
//! can be reproduced from the seed alone and so that the choice of what to test
//! can be argued about, and unit-tested, without a simulator anywhere near it.
//!
//! # Why the configuration is drawn and not written down
//!
//! The obvious way to configure a fault-injection run is a handful of TOML
//! files, one per scenario. That gives you exactly as many configurations as
//! someone bothered to write, and the interesting failures live in the
//! combinations nobody thought of: a resigning leader whose clock jumps, in a
//! run whose lease is short enough that the campaign backoff and the lease
//! expiry land on the same step. Swarm testing (Groce et al., ISSTA 2012) is
//! the answer to that: instead of enabling every feature in every run, draw a
//! random *subset* of features per run. Runs with fewer features enabled go
//! deeper into the code paths those features reach, and across many seeds the
//! suite covers combinations no author enumerated.
//!
//! The anchor configurations stay. They are the ones a human reasoned about,
//! and they are what a reader looks at to understand what the workload does.
//! Swarm runs are what finds the case the reader did not think of.
//!
//! # Why the feature subset is not five coin flips
//!
//! Five independent fair coins is the naive way to draw a subset, and it is a
//! bad one: the number of enabled features is binomial, so it concentrates
//! around two and a half. Everything enabled and nothing enabled each happen
//! one run in thirty-two, and a subset with exactly one feature enabled, which
//! is the configuration that isolates that feature's code path best, happens
//! about one run in six spread over five different features. The extremes are
//! exactly the configurations worth oversampling, so [`SwarmPlan::draw`] picks
//! the *shape* of the subset first from a fat-tailed selector, and only falls
//! back to coins in the remaining half of the probability mass.
//!
//! # Why faults come in storms
//!
//! The workload this replaces rolled one coin per client per step against a
//! fixed probability. That produces faults spread uniformly over the run, which
//! is the one arrival pattern real outages never have, and it makes two
//! situations essentially unreachable: a burst dense enough that a replacement
//! leader crashes before it finishes claiming, and a quiet tail long enough to
//! prove the system actually recovers. [`FaultTiming::Storms`] draws a handful
//! of windows instead, each with its own intensity, and leaves the last third
//! of the run fault-free so that the progress invariants are checking recovery
//! rather than luck. [`FaultTiming::Constant`] is kept because the anchor
//! configurations are specified in terms of a per-step probability, and it must
//! behave exactly like the coin they were written against.
//!
//! # Lane partitioning
//!
//! Every section of the draw gets its own generator, seeded from the run seed
//! and a lane constant. This is a contract, not an implementation detail:
//! adding a draw to one section never shifts the values another section
//! produces. Without it, every change to this file would silently reshuffle
//! every seed's configuration, and the seed in a bug report would stop meaning
//! anything the day someone added a knob. Within a lane the draw order is
//! fixed and documented, and branches are written to consume the same number of
//! values on both sides wherever that is cheap.

// The plan is consumed by the workload, which lands separately; until then the
// only callers are this module's tests.
#![allow(dead_code)]

use std::time::Duration;

use super::clock::SkewMode;
use super::invariants::ProgressThresholds;

// ============================================================================
// GENERATOR
// ============================================================================

/// Golden-ratio odd constant, the SplitMix64 increment
const GOLDEN: u64 = 0x9E37_79B9_7F4A_7C15;

/// The lane the feature subset is drawn from
const LANE_FEATURES: u64 = 1;
/// The lane the clock skew mode is drawn from
const LANE_SKEW: u64 = 2;
/// The lane the timing knobs are drawn from
const LANE_KNOBS: u64 = 3;
/// The lane the crash storms are drawn from
const LANE_CRASH_STORMS: u64 = 4;
/// The lane the resign storms are drawn from
const LANE_RESIGN_STORMS: u64 = 5;

/// A deterministic generator for one lane of the draw
///
/// SplitMix64, the same finalizer the recipe's campaign jitter uses: pure,
/// dependency-free, and identical across builds and platforms, which is what
/// makes a seed in a bug report reproduce the run it came from.
#[derive(Debug, Clone)]
pub(crate) struct SwarmRng {
    state: u64,
}

impl SwarmRng {
    /// The generator for `lane` of the draw for `seed`
    ///
    /// The lane is mixed in before the first output rather than added to the
    /// stream position, so two lanes of the same seed are unrelated sequences
    /// instead of the same sequence at different offsets.
    pub(crate) fn lane(seed: u64, lane: u64) -> Self {
        Self {
            state: mix(seed ^ lane.wrapping_mul(GOLDEN)),
        }
    }

    /// The next value in this lane
    pub(crate) fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(GOLDEN);
        mix(self.state)
    }

    /// The next value as a float in `[0, 1)`
    ///
    /// Built from the top 53 bits, which is every bit an `f64` can hold, so the
    /// result is uniform over the representable values rather than over a
    /// coarser grid.
    pub(crate) fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// A uniform choice from `values`
    ///
    /// # Panics
    ///
    /// If `values` is empty. Every call site here passes a literal palette.
    pub(crate) fn pick<T: Copy>(&mut self, values: &[T]) -> T {
        let index = (self.next() % values.len() as u64) as usize;
        values[index]
    }

    /// A uniform value in `[lo, hi)`
    pub(crate) fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + self.unit() * (hi - lo)
    }
}

/// The SplitMix64 finalizer
fn mix(value: u64) -> u64 {
    let mut z = value;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

// ============================================================================
// FEATURES
// ============================================================================

/// How many features the subset is drawn over
const FEATURE_COUNT: usize = 5;

/// Which behaviours a run is allowed to exercise
///
/// A feature being off means the run cannot produce that behaviour at all, not
/// that it is unlikely to: that is what makes a run with one feature enabled a
/// deeper test of that feature's code path than a run with all five.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FeatureSet {
    /// Leaders may hand their term back voluntarily
    pub(crate) resign: bool,
    /// Leaders may stop responding for longer than their lease
    pub(crate) crash: bool,
    /// One client may pause across a lease boundary
    pub(crate) sleeper: bool,
    /// Some clients only watch, never campaign
    pub(crate) watcher: bool,
    /// Clients' clocks may disagree
    pub(crate) skew: bool,
}

impl FeatureSet {
    /// The subset with every feature enabled
    pub(crate) const ALL: Self = Self {
        resign: true,
        crash: true,
        sleeper: true,
        watcher: true,
        skew: true,
    };

    /// The subset with nothing enabled
    pub(crate) const NONE: Self = Self {
        resign: false,
        crash: false,
        sleeper: false,
        watcher: false,
        skew: false,
    };

    /// Rebuild a subset from the bit order the draw uses
    fn from_bits(bits: [bool; FEATURE_COUNT]) -> Self {
        Self {
            resign: bits[0],
            crash: bits[1],
            sleeper: bits[2],
            watcher: bits[3],
            skew: bits[4],
        }
    }

    /// This subset in the bit order the draw uses
    fn bits(self) -> [bool; FEATURE_COUNT] {
        [
            self.resign,
            self.crash,
            self.sleeper,
            self.watcher,
            self.skew,
        ]
    }

    /// How many features are enabled
    pub(crate) fn enabled(self) -> usize {
        self.bits().iter().filter(|bit| **bit).count()
    }
}

// ============================================================================
// FAULT TIMING
// ============================================================================

/// One window during which a fault is injected at a fixed rate
///
/// Times are elapsed simulated time since the start phase began, which is what
/// the driver measures its own progress in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Storm {
    /// When the window opens
    pub(crate) start: Duration,
    /// How long it stays open
    pub(crate) len: Duration,
    /// The per-step probability inside it
    pub(crate) intensity: f64,
}

impl Storm {
    /// When the window closes; the window itself is half-open
    pub(crate) fn end(&self) -> Duration {
        self.start + self.len
    }

    /// Whether `elapsed` falls inside the window
    fn covers(&self, elapsed: Duration) -> bool {
        elapsed >= self.start && elapsed < self.end()
    }
}

/// How a fault's probability varies over a run
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FaultTiming {
    /// The same probability at every step, for the whole run
    ///
    /// This is the anchor-configuration path, and it must behave exactly like
    /// the per-step coin the anchor TOMLs were written against: a run
    /// configured with a probability of `p` sees `p` at every instant, with no
    /// windows, no ramps and no quiet tail.
    Constant(f64),
    /// Bursts, with a fault-free tail after the last one
    ///
    /// Windows are allowed to overlap. When they do the highest intensity wins
    /// rather than the probabilities compounding, so an overlap can only make
    /// the run as bad as its worst constituent storm, which keeps the injected
    /// rate inside the palette the storms were drawn from.
    Storms(Vec<Storm>),
}

impl FaultTiming {
    /// The timing of a fault that never fires
    pub(crate) fn quiet() -> Self {
        Self::Constant(0.0)
    }

    /// The per-step probability at `elapsed_sim` into the start phase
    pub(crate) fn probability_at(&self, elapsed_sim: Duration) -> f64 {
        match self {
            Self::Constant(probability) => *probability,
            Self::Storms(storms) => storms
                .iter()
                .filter(|storm| storm.covers(elapsed_sim))
                .fold(0.0, |worst, storm| worst.max(storm.intensity)),
        }
    }

    /// The windows, for a trace of the plan; empty for a constant rate
    fn storms(&self) -> &[Storm] {
        match self {
            Self::Constant(_) => &[],
            Self::Storms(storms) => storms,
        }
    }
}

// ============================================================================
// PLAN
// ============================================================================

/// The lease durations the draw prefers, in seconds
///
/// Capped at sixteen because of the recovery tail: storms stop two thirds of
/// the way through the run, and what is left has to fit two full observation
/// windows for the progress invariants to mean anything. In a hundred and
/// twenty second run that tail is about forty seconds, which is two and a half
/// leases at the cap and less than two at any larger value.
const LEASE_PALETTE: [f64; 6] = [1.0, 2.0, 3.0, 4.0, 8.0, 16.0];
/// The step intervals the draw prefers, in seconds
const STEP_PALETTE: [f64; 4] = [0.25, 0.5, 1.0, 2.0];
/// The sleeper pause lengths the draw prefers, in leases
const PAUSE_PALETTE: [f64; 3] = [1.5, 2.0, 3.0];
/// The storm lengths the draw prefers, in leases
const STORM_LEN_PALETTE: [f64; 3] = [1.0, 2.0, 4.0];
/// The per-step probabilities a storm may run at
const STORM_INTENSITY_PALETTE: [f64; 3] = [0.25, 0.6, 1.0];
/// The fraction of the run faults are allowed to happen in
///
/// The remaining third is the recovery tail: no fault starts in it, so a run
/// that fails a progress invariant failed to recover rather than failing to
/// catch a break.
const ACTIVE_FRACTION: f64 = 2.0 / 3.0;
/// The fraction of the run a sleeper's pause is allowed to occupy
const PAUSE_BUDGET_FRACTION: f64 = 1.0 / 3.0;

/// Everything one seed decides about a run
///
/// A plan is a value: it holds no clock, no generator and no simulator handle,
/// and equal plans configure identical runs. That is what lets the check phase
/// print [`describe`](Self::describe) into the trace and have it be a complete
/// reproduction recipe.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SwarmPlan {
    /// The seed this plan was drawn from
    pub(crate) seed: u64,
    /// Which behaviours the run may exercise
    pub(crate) features: FeatureSet,
    /// What the clocks are allowed to do
    pub(crate) skew_mode: SkewMode,
    /// The lease every claim advertises, in seconds
    pub(crate) lease_secs: f64,
    /// How long a client waits between actions, in seconds
    pub(crate) step_secs: f64,
    /// How many leases the sleeper pauses for
    pub(crate) pause_factor: f64,
    /// When leaders stop responding
    pub(crate) crash: FaultTiming,
    /// When leaders hand their term back
    pub(crate) resign: FaultTiming,
}

impl SwarmPlan {
    /// Draw the plan for `seed` in a run of `test_duration`
    ///
    /// The draw order below is part of the contract described in the module
    /// documentation. Sections may gain draws at their end without disturbing
    /// other sections, because each section reads its own lane.
    pub(crate) fn draw(seed: u64, test_duration: Duration) -> Self {
        let features = draw_features(seed);
        let skew_mode = draw_skew_mode(seed, features);
        let (lease_secs, step_secs, pause_factor) = draw_knobs(seed, test_duration);

        let crash = draw_storms(
            seed,
            LANE_CRASH_STORMS,
            features.crash,
            lease_secs,
            test_duration,
        );
        let resign = draw_storms(
            seed,
            LANE_RESIGN_STORMS,
            features.resign,
            lease_secs,
            test_duration,
        );

        Self {
            seed,
            features,
            skew_mode,
            lease_secs,
            step_secs,
            pause_factor,
            crash,
            resign,
        }
    }

    /// The lease this plan configures
    pub(crate) fn lease(&self) -> Duration {
        Duration::from_secs_f64(self.lease_secs)
    }

    /// How long a client waits between actions
    pub(crate) fn step(&self) -> Duration {
        Duration::from_secs_f64(self.step_secs)
    }

    /// What this run has to have achieved to count as having tested anything
    ///
    /// The thresholds follow the churn the plan actually configured. A run with
    /// no faults enabled only has to prove one client took the lease and kept
    /// it, and demanding more of it would fail honest runs; a run with three
    /// sources of churn has to show the record changed hands for each of them,
    /// and demanding less would let a run pass having injected faults nobody
    /// reacted to.
    pub(crate) fn thresholds(&self, client_count: i32) -> ProgressThresholds {
        // A sleeper only produces churn if somebody else is around to take the
        // record while it sleeps, and the role assignment does not hand out a
        // sleeper below three clients in the first place.
        let sleeper_active = self.features.sleeper && client_count >= 3;
        let churn = usize::from(self.features.crash)
            + usize::from(self.features.resign)
            + usize::from(sleeper_active);

        ProgressThresholds {
            // One acquisition for the opening claim, and one more for each way
            // this plan can take the record away from whoever holds it.
            min_acquisitions: 1 + churn,
            // Flat, and deliberately not scaled by churn: the renewal check is
            // already conditional on a belief having outlived its renewal
            // deadline, so it demands renewals only of runs that had the
            // chance to make them. Scaling this number would demand renewals
            // from runs whose leaders were killed before the deadline, which
            // is precisely the run a churn-heavy plan is trying to produce.
            min_renewals: 2,
            // The two clients the opening handover involves, plus one per
            // source of churn, capped: past five the bound stops describing
            // the plan and starts describing how many clients the simulator
            // happened to give us.
            min_observed_identities: (2 + churn).min(5),
            renew_interval: self.lease() / 3,
        }
    }

    /// This plan on one line, in full
    ///
    /// Written into the simulation trace, and the reason a seed in a failure
    /// report is actionable: everything the run does is here, so a reader can
    /// tell a plan that never enabled crashes from a run whose crashes never
    /// fired.
    pub(crate) fn describe(&self) -> String {
        format!(
            "seed={} features=resign:{} crash:{} sleeper:{} watcher:{} skew:{} \
             skewMode={} lease={:.3}s step={:.3}s pause={:.2}x crash={} resign={}",
            self.seed,
            on_off(self.features.resign),
            on_off(self.features.crash),
            on_off(self.features.sleeper),
            on_off(self.features.watcher),
            on_off(self.features.skew),
            self.skew_mode.as_str(),
            self.lease_secs,
            self.step_secs,
            self.pause_factor,
            describe_timing(&self.crash),
            describe_timing(&self.resign),
        )
    }
}

/// How a feature is rendered in a plan description
fn on_off(enabled: bool) -> &'static str {
    if enabled { "on" } else { "off" }
}

/// How a fault timing is rendered in a plan description
fn describe_timing(timing: &FaultTiming) -> String {
    match timing {
        FaultTiming::Constant(probability) => format!("constant({probability:.3})"),
        FaultTiming::Storms(storms) => {
            let windows: Vec<String> = storms
                .iter()
                .map(|storm| {
                    format!(
                        "{:.2}s+{:.2}s@{:.2}",
                        storm.start.as_secs_f64(),
                        storm.len.as_secs_f64(),
                        storm.intensity
                    )
                })
                .collect();
            format!("storms[{}]", windows.join(";"))
        }
    }
}

// ============================================================================
// THE DRAW, LANE BY LANE
// ============================================================================

/// Draw the feature subset
///
/// The selector is fat-tailed on purpose: see the module documentation for why
/// five coins on their own would almost never produce the subsets worth having.
/// The order is one selector draw, then whatever the chosen shape needs.
fn draw_features(seed: u64) -> FeatureSet {
    let mut rng = SwarmRng::lane(seed, LANE_FEATURES);
    let shape = rng.unit();

    if shape < 0.10 {
        FeatureSet::ALL
    } else if shape < 0.20 {
        FeatureSet::NONE
    } else if shape < 0.35 {
        // Exactly one feature enabled: the deepest test of that one path.
        let index = (rng.next() % FEATURE_COUNT as u64) as usize;
        let mut bits = [false; FEATURE_COUNT];
        bits[index] = true;
        FeatureSet::from_bits(bits)
    } else if shape < 0.45 {
        // Exactly one feature disabled: what breaks when this one thing is
        // absent from an otherwise busy run.
        let index = (rng.next() % FEATURE_COUNT as u64) as usize;
        let mut bits = [true; FEATURE_COUNT];
        bits[index] = false;
        FeatureSet::from_bits(bits)
    } else {
        let mut bits = [false; FEATURE_COUNT];
        for bit in &mut bits {
            *bit = rng.unit() < 0.5;
        }
        FeatureSet::from_bits(bits)
    }
}

/// Draw the clock skew mode
///
/// The mode is a consequence of the skew feature, but it reads its own lane so
/// that adding a third skew mode does not move any other section's values.
fn draw_skew_mode(seed: u64, features: FeatureSet) -> SkewMode {
    let mut rng = SwarmRng::lane(seed, LANE_SKEW);
    let choice = rng.unit();
    if features.skew {
        if choice < 0.5 {
            SkewMode::Random
        } else {
            SkewMode::Extreme
        }
    } else {
        SkewMode::None
    }
}

/// Draw the timing knobs: lease, step, pause
///
/// Order: lease selector, lease value, step, pause. Both lease branches consume
/// exactly one value after the selector, so which branch was taken does not
/// shift the step and pause draws.
fn draw_knobs(seed: u64, test_duration: Duration) -> (f64, f64, f64) {
    let mut rng = SwarmRng::lane(seed, LANE_KNOBS);

    // Mostly the palette, because those are the values whose interactions with
    // the step interval are worth hitting repeatedly, with a uniform tail so
    // that no ratio between lease and step is structurally unreachable.
    let lease_secs = if rng.unit() < 0.70 {
        rng.pick(&LEASE_PALETTE)
    } else {
        rng.range(LEASE_PALETTE[0], LEASE_PALETTE[LEASE_PALETTE.len() - 1])
    };

    // At least four steps per lease, so a leader always gets the chance to
    // notice its own renewal deadline before the lease runs out.
    let step_secs = rng.pick(&STEP_PALETTE).min(lease_secs / 4.0);

    // Always drawn, even when the sleeper feature is off, so that turning the
    // feature on does not move anything else in this lane. The workload
    // consumes it only when the feature is enabled.
    let pause_factor = rng.pick(&PAUSE_PALETTE);
    // A pause that eats the whole run leaves nothing to recover in.
    let pause_budget = test_duration.as_secs_f64() * PAUSE_BUDGET_FRACTION;
    let pause_factor = pause_factor.min(pause_budget / lease_secs);

    (lease_secs, step_secs, pause_factor)
}

/// Draw the storms for one fault, or silence if its feature is off
///
/// Order per storm: length, intensity, start. The start draw happens even when
/// the storm had to be truncated to fit, so the number of values a storm
/// consumes does not depend on the lease.
fn draw_storms(
    seed: u64,
    lane: u64,
    enabled: bool,
    lease_secs: f64,
    test_duration: Duration,
) -> FaultTiming {
    if !enabled {
        return FaultTiming::quiet();
    }

    let mut rng = SwarmRng::lane(seed, lane);
    let active_deadline = test_duration.mul_f64(ACTIVE_FRACTION);

    // Weighted towards a single storm: one long burst followed by a long
    // recovery is the shape that tests recovery hardest, and three overlapping
    // ones mostly test the simulator's patience.
    let weight = rng.unit();
    let count = if weight < 0.5 {
        1
    } else if weight < 0.8 {
        2
    } else {
        3
    };

    let storms = (0..count)
        .map(|_| {
            // A storm shorter than a lease cannot outlast the term it
            // interrupts, so the palette starts at one lease.
            let len = Duration::from_secs_f64(lease_secs * rng.pick(&STORM_LEN_PALETTE))
                .min(active_deadline);
            let intensity = rng.pick(&STORM_INTENSITY_PALETTE);
            let span = active_deadline.saturating_sub(len);
            // The `min` is not redundant: `mul_f64` rounds, and a storm that
            // ended one nanosecond into the recovery tail would break the
            // property the tail exists for.
            let start = span.mul_f64(rng.unit()).min(span);
            Storm {
                start,
                len,
                intensity,
            }
        })
        .collect();

    FaultTiming::Storms(storms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// The run length the anchor configurations use
    const RUN: Duration = Duration::from_secs(120);
    /// How many seeds a distribution test looks at
    const SAMPLES: u64 = 10_000;

    /// Every plan in the sample, in seed order
    fn sample() -> Vec<SwarmPlan> {
        (0..SAMPLES)
            .map(|seed| SwarmPlan::draw(seed, RUN))
            .collect()
    }

    #[test]
    fn same_seed_draws_the_same_plan() {
        for seed in [0u64, 1, 7, 4242, u64::MAX / 3, u64::MAX] {
            let first = SwarmPlan::draw(seed, RUN);
            let second = SwarmPlan::draw(seed, RUN);
            assert_eq!(first, second, "seed {seed} drew two different plans");
        }
    }

    #[test]
    fn every_feature_is_hard_off_and_on_in_a_nontrivial_fraction() {
        // A feature that is almost always on is never tested in isolation, and
        // one that is almost always off is never tested at all. Both halves
        // have to be common enough that a nightly run hits them.
        let plans = sample();
        let names = ["resign", "crash", "sleeper", "watcher", "skew"];
        for (index, name) in names.iter().enumerate() {
            let on = plans
                .iter()
                .filter(|plan| plan.features.bits()[index])
                .count();
            let off = plans.len() - on;
            let floor = plans.len() * 15 / 100;
            assert!(on >= floor, "{name} was enabled in only {on} of {SAMPLES}");
            assert!(
                off >= floor,
                "{name} was disabled in only {off} of {SAMPLES}"
            );
        }
    }

    #[test]
    fn extreme_subsets_are_oversampled() {
        // The whole point of the fat-tailed selector: five fair coins would put
        // each extreme at about three percent.
        let plans = sample();
        let all = plans
            .iter()
            .filter(|plan| plan.features == FeatureSet::ALL)
            .count();
        let none = plans
            .iter()
            .filter(|plan| plan.features == FeatureSet::NONE)
            .count();
        let floor = plans.len() * 5 / 100;
        assert!(
            all >= floor,
            "everything enabled in only {all} of {SAMPLES}"
        );
        assert!(none >= floor, "nothing enabled in only {none} of {SAMPLES}");

        for index in 0..FEATURE_COUNT {
            let mut bits = [false; FEATURE_COUNT];
            bits[index] = true;
            let wanted = FeatureSet::from_bits(bits);
            assert!(
                plans.iter().any(|plan| plan.features == wanted),
                "no seed isolated feature {index} on its own"
            );
        }
    }

    #[test]
    fn lease_palette_boundaries_are_hit() {
        let plans = sample();
        let leases: Vec<f64> = plans.iter().map(|plan| plan.lease_secs).collect();
        assert!(leases.iter().any(|lease| *lease == LEASE_PALETTE[0]));
        assert!(leases.iter().any(|lease| *lease == LEASE_PALETTE[5]));
        assert!(
            leases.iter().any(|lease| !LEASE_PALETTE.contains(lease)),
            "the uniform tail never produced an off-palette lease"
        );
        for lease in leases {
            assert!(
                (LEASE_PALETTE[0]..=LEASE_PALETTE[5]).contains(&lease),
                "lease {lease} left the drawable range"
            );
        }
    }

    #[test]
    fn step_never_exceeds_a_quarter_lease() {
        // Fewer than four steps per lease and a leader can sleep through its
        // own renewal deadline, which would fail the progress invariants for a
        // reason that says nothing about the recipe.
        for plan in sample() {
            assert!(
                plan.step_secs <= plan.lease_secs / 4.0,
                "step {} against lease {}",
                plan.step_secs,
                plan.lease_secs
            );
        }
    }

    #[test]
    fn pause_fits_inside_the_run() {
        let budget = RUN.as_secs_f64() * PAUSE_BUDGET_FRACTION;
        for plan in sample() {
            let pause = plan.pause_factor * plan.lease_secs;
            // The clamp divides then the check multiplies, so a last-bit
            // rounding difference is expected and harmless; anything larger is
            // a plan that sleeps through its own run.
            assert!(
                pause <= budget + 1e-9,
                "pause of {pause}s against a budget of {budget}s"
            );
            assert!(
                plan.pause_factor > 0.0,
                "a pause of zero leases is no pause"
            );
        }
    }

    #[test]
    fn storms_leave_the_recovery_tail_fault_free() {
        let deadline = RUN.mul_f64(ACTIVE_FRACTION);
        for plan in sample() {
            for timing in [&plan.crash, &plan.resign] {
                for storm in timing.storms() {
                    assert!(
                        storm.end() <= deadline,
                        "a storm ran to {:?}, past the {deadline:?} deadline",
                        storm.end()
                    );
                }
                // Sampled rather than proved, because the property that
                // matters to the invariants is what the driver sees when it
                // asks.
                for tenth in 0..10 {
                    let elapsed = deadline + (RUN - deadline).mul_f64(f64::from(tenth) / 10.0);
                    assert_eq!(
                        timing.probability_at(elapsed),
                        0.0,
                        "the tail was not quiet at {elapsed:?} for seed {}",
                        plan.seed
                    );
                }
            }
        }
    }

    #[test]
    fn a_disabled_feature_has_no_storms() {
        // Off must mean unreachable, not unlikely: that is what makes a
        // one-feature plan a deeper test of that feature.
        let mut checked_crash = 0;
        let mut checked_resign = 0;
        for plan in sample() {
            let windows: Vec<Duration> = plan
                .crash
                .storms()
                .iter()
                .chain(plan.resign.storms())
                .map(|storm| storm.start + storm.len / 2)
                .collect();
            let probes: Vec<Duration> = (0..=120).map(Duration::from_secs).chain(windows).collect();

            if !plan.features.crash {
                checked_crash += 1;
                assert!(plan.crash.storms().is_empty());
                for probe in &probes {
                    assert_eq!(plan.crash.probability_at(*probe), 0.0);
                }
            }
            if !plan.features.resign {
                checked_resign += 1;
                assert!(plan.resign.storms().is_empty());
                for probe in &probes {
                    assert_eq!(plan.resign.probability_at(*probe), 0.0);
                }
            }
        }
        assert!(
            checked_crash > 0 && checked_resign > 0,
            "nothing was checked"
        );
    }

    #[test]
    fn storm_windows_carry_their_intensity() {
        let storms = FaultTiming::Storms(vec![
            Storm {
                start: Duration::from_secs(10),
                len: Duration::from_secs(10),
                intensity: 0.25,
            },
            Storm {
                start: Duration::from_secs(15),
                len: Duration::from_secs(10),
                intensity: 1.0,
            },
            Storm {
                start: Duration::from_secs(40),
                len: Duration::from_secs(5),
                intensity: 0.6,
            },
        ]);

        assert_eq!(storms.probability_at(Duration::from_secs(0)), 0.0);
        assert_eq!(storms.probability_at(Duration::from_secs(12)), 0.25);
        // Overlap: the worst of the two, not their sum.
        assert_eq!(storms.probability_at(Duration::from_secs(17)), 1.0);
        assert_eq!(storms.probability_at(Duration::from_secs(22)), 1.0);
        // The window is half-open, so its own end is already outside.
        assert_eq!(storms.probability_at(Duration::from_secs(25)), 0.0);
        assert_eq!(storms.probability_at(Duration::from_secs(30)), 0.0);
        assert_eq!(storms.probability_at(Duration::from_secs(42)), 0.6);
        assert_eq!(storms.probability_at(Duration::from_secs(45)), 0.0);
    }

    #[test]
    fn constant_timing_matches_the_legacy_coin() {
        // The anchor configurations name a per-step probability, and this path
        // has to be that probability at every instant of the run.
        for probability in [0.0, 0.05, 0.5, 1.0] {
            let timing = FaultTiming::Constant(probability);
            for secs in [0u64, 1, 40, 79, 80, 119, 10_000] {
                assert_eq!(
                    timing.probability_at(Duration::from_secs(secs)),
                    probability
                );
            }
            assert!(timing.storms().is_empty());
        }
        assert_eq!(
            FaultTiming::quiet().probability_at(Duration::from_secs(3)),
            0.0
        );
    }

    #[test]
    fn thresholds_are_never_vacuous() {
        // A run that has to prove nothing is a run that passes every safety
        // check by never doing anything.
        for plan in sample() {
            for clients in 1..=8 {
                let thresholds = plan.thresholds(clients);
                assert!(thresholds.min_acquisitions >= 1);
                assert!(thresholds.min_observed_identities >= 2);
                assert!(thresholds.renew_interval > Duration::ZERO);
            }
        }
    }

    #[test]
    fn thresholds_follow_the_drawn_churn() {
        let base = SwarmPlan::draw(0, RUN);
        let with = |features: FeatureSet| SwarmPlan {
            features,
            lease_secs: 6.0,
            ..base.clone()
        };

        let quiet = with(FeatureSet::NONE).thresholds(8);
        assert_eq!(quiet.min_acquisitions, 1);
        assert_eq!(quiet.min_renewals, 2);
        assert_eq!(quiet.min_observed_identities, 2);
        assert_eq!(quiet.renew_interval, Duration::from_secs(2));

        for feature in ["crash", "resign", "sleeper"] {
            let mut features = FeatureSet::NONE;
            match feature {
                "crash" => features.crash = true,
                "resign" => features.resign = true,
                _ => features.sleeper = true,
            }
            let thresholds = with(features).thresholds(8);
            assert_eq!(
                thresholds.min_acquisitions, 2,
                "{feature} did not add an acquisition"
            );
            assert_eq!(
                thresholds.min_observed_identities, 3,
                "{feature} did not add an identity"
            );
            assert_eq!(thresholds.min_renewals, 2, "{feature} scaled the renewals");
        }

        // A sleeper with nobody to take over from it produces no churn.
        let mut sleeper = FeatureSet::NONE;
        sleeper.sleeper = true;
        for clients in 1..=2 {
            let thresholds = with(sleeper).thresholds(clients);
            assert_eq!(thresholds.min_acquisitions, 1);
            assert_eq!(thresholds.min_observed_identities, 2);
        }

        // Everything at once, and the identity bound stops at five.
        let busy = with(FeatureSet::ALL).thresholds(8);
        assert_eq!(busy.min_acquisitions, 4);
        assert_eq!(busy.min_observed_identities, 5);
    }

    #[test]
    fn skew_mode_follows_the_skew_feature() {
        let mut random = 0;
        let mut extreme = 0;
        for plan in sample() {
            if plan.features.skew {
                match plan.skew_mode {
                    SkewMode::Random => random += 1,
                    SkewMode::Extreme => extreme += 1,
                    SkewMode::None => panic!("the skew feature drew an unskewed clock"),
                }
            } else {
                assert_eq!(
                    plan.skew_mode,
                    SkewMode::None,
                    "seed {} skewed clocks with the feature off",
                    plan.seed
                );
            }
        }
        assert!(random > 0 && extreme > 0, "one skew mode was never drawn");
    }

    #[test]
    fn the_plan_description_states_every_field() {
        // Find a seed whose plan exercises both fault paths, so the rendering
        // of a storm list is covered and not just the constant one.
        let plan = sample()
            .into_iter()
            .find(|plan| plan.features.crash && plan.features.resign)
            .expect("no seed enabled both faults");
        let text = plan.describe();

        for fragment in [
            "seed=",
            "features=",
            "resign:",
            "crash:",
            "sleeper:",
            "watcher:",
            "skew:",
            "skewMode=",
            "lease=",
            "step=",
            "pause=",
            "storms[",
        ] {
            assert!(
                text.contains(fragment),
                "{fragment:?} missing from {text:?}"
            );
        }
        assert!(text.contains(&plan.seed.to_string()));
        assert!(
            !text.contains('\n'),
            "the description must stay on one line"
        );

        for storm in plan.crash.storms() {
            let rendered = format!(
                "{:.2}s+{:.2}s",
                storm.start.as_secs_f64(),
                storm.len.as_secs_f64()
            );
            assert!(text.contains(&rendered), "{rendered} missing from {text:?}");
        }
    }

    #[test]
    fn lanes_stay_isolated() {
        // The contract: a section's values depend on its own lane and nothing
        // else. `test_duration` only ever reaches the knob and storm lanes, so
        // changing it must leave the feature and skew lanes alone, and must
        // leave the lease and step alone even though they share a lane with the
        // pause, which does read it.
        let short = Duration::from_secs(60);
        let long = Duration::from_secs(600);
        let mut pause_differed = 0;
        for seed in 0..SAMPLES {
            let a = SwarmPlan::draw(seed, short);
            let b = SwarmPlan::draw(seed, long);
            assert_eq!(a.features, b.features, "seed {seed} shifted its features");
            assert_eq!(a.skew_mode, b.skew_mode, "seed {seed} shifted its skew");
            assert_eq!(a.lease_secs, b.lease_secs, "seed {seed} shifted its lease");
            assert_eq!(a.step_secs, b.step_secs, "seed {seed} shifted its step");
            if a.pause_factor != b.pause_factor {
                pause_differed += 1;
            }
        }
        // And the pause really does depend on the run length, so the assertions
        // above are not passing because nothing reads `test_duration`.
        assert!(
            pause_differed > 0,
            "the pause clamp never noticed the run length"
        );

        // Storm shape depends on its own lane only: two lanes of the same seed
        // are unrelated, not one sequence at two offsets.
        let stream = |lane: u64| {
            let mut rng = SwarmRng::lane(1234, lane);
            (0..8).map(|_| rng.next()).collect::<Vec<u64>>()
        };
        assert_ne!(stream(LANE_CRASH_STORMS), stream(LANE_RESIGN_STORMS));
        let distinct: HashSet<u64> = [
            LANE_FEATURES,
            LANE_SKEW,
            LANE_KNOBS,
            LANE_CRASH_STORMS,
            LANE_RESIGN_STORMS,
        ]
        .into_iter()
        .collect();
        assert_eq!(distinct.len(), 5, "two sections share a lane constant");
    }
}
