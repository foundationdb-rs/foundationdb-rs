//! The twelve properties a run has to satisfy.
//!
//! Each one is a pure function of what [`replay`](super::replay) reconstructed,
//! plus whatever external evidence it needs (the database snapshot, the
//! recipe's own history subspace, the tolerances the configuration implies).
//! None of them touch the database, a clock, or the simulator, so every one of
//! them is tested below against a hand-mutated log that must make it fail.
//!
//! That last part is the point of this module. The suite it replaces had seven
//! invariants that could not fail for any input, which meant the simulation was
//! reporting success it had never checked. A check here earns its place only
//! with a counterexample, and every counterexample is a test.
//!
//! # Tolerances
//!
//! Two checks compare durations that were measured on different clocks. Clock
//! *offset* cancels out of an elapsed-time measurement, so only the rate error
//! matters, and it accumulates with the interval measured: over an interval of
//! length `L`, two clocks within a relative rate error `e` of true time can
//! disagree by up to `L * 2e / (1 + e)`. [`Tolerances::from_clock_rate_error`]
//! is that formula, and the strict zero-skew configuration uses
//! [`Tolerances::STRICT`], where the tolerance is exactly zero and any slack at
//! all is a violation.

use std::collections::HashMap;
use std::time::Duration;

use super::log_schema::{LogEntry, LogRecord, OpKind};
use super::replay::{ExpectedRecord, Replay, TransitionKind};

// ============================================================================
// RESULTS
// ============================================================================

/// One way a run broke an invariant
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// Indices of the offending log entries, in commit order
    pub indices: Vec<usize>,
    /// What went wrong, in terms a reader of the trace can act on
    pub detail: String,
}

impl Violation {
    pub(crate) fn at(index: usize, detail: impl Into<String>) -> Self {
        Self {
            indices: vec![index],
            detail: detail.into(),
        }
    }

    pub(crate) fn spanning(indices: Vec<usize>, detail: impl Into<String>) -> Self {
        Self {
            indices,
            detail: detail.into(),
        }
    }

    pub(crate) fn global(detail: impl Into<String>) -> Self {
        Self {
            indices: Vec::new(),
            detail: detail.into(),
        }
    }
}

/// What checking one invariant produced
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvariantReport {
    /// The invariant's name, as it appears in the simulation trace
    pub name: &'static str,
    /// Everything that broke it; empty means it held
    pub violations: Vec<Violation>,
}

impl InvariantReport {
    pub(crate) fn new(name: &'static str, violations: Vec<Violation>) -> Self {
        Self { name, violations }
    }

    /// Whether the invariant held
    pub fn passed(&self) -> bool {
        self.violations.is_empty()
    }
}

/// How many violations of one invariant are kept
///
/// The checks that pair every belief with every other, or every steal with
/// everything inside its window, are quadratic in the log. A run in which
/// something went badly wrong produces violations by the same square, and
/// keeping them all means the check phase that was supposed to report the
/// failure dies of memory instead.
///
/// Sixty-four is far more than anybody reads (the workload spells out five and
/// summarises the rest) and small enough to be free. Nothing about the judgement
/// changes: a report is failed by having any violation at all, so a capped
/// report fails exactly as hard as an uncapped one, and the count of what was
/// dropped is reported alongside.
const MAX_VIOLATIONS_KEPT: usize = 64;

/// How many log indices one violation may name
///
/// The same reasoning one level down: a steal whose observation window was
/// interrupted names every interfering write, and a runaway run interferes with
/// itself thousands of times over.
const MAX_INDICES_KEPT: usize = 64;

/// A bounded collector for the checks that can produce a violation per pair
///
/// Push freely; what comes out is at most [`MAX_VIOLATIONS_KEPT`] violations
/// plus, when there were more, one final entry counting the rest. That last
/// entry is a violation like any other, so a capped report still fails the run.
#[derive(Debug, Default)]
pub(crate) struct Violations {
    kept: Vec<Violation>,
    total: usize,
}

impl Violations {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn push(&mut self, violation: Violation) {
        self.total += 1;
        if self.kept.len() < MAX_VIOLATIONS_KEPT {
            self.kept.push(violation);
        }
    }

    /// The indices of one violation, bounded the same way
    pub(crate) fn indices(mut indices: Vec<usize>) -> Vec<usize> {
        indices.truncate(MAX_INDICES_KEPT);
        indices
    }

    pub(crate) fn into_report(mut self, name: &'static str) -> InvariantReport {
        let dropped = self.total.saturating_sub(self.kept.len());
        if dropped > 0 {
            self.kept.push(Violation::global(format!(
                "and {dropped} further violation(s), not spelled out: \
                 {} were found in total",
                self.total
            )));
        }
        InvariantReport::new(name, self.kept)
    }
}

// ============================================================================
// PARAMETERS
// ============================================================================

/// How much clock disagreement a configuration admits
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tolerances {
    /// How far two belief intervals may overlap before it counts
    pub belief_overlap: Duration,
    /// How much shorter than a full lease an observation window may measure
    pub observation_slack: Duration,
}

impl Tolerances {
    /// No slack whatsoever: the configuration with identical clocks
    pub const STRICT: Self = Self {
        belief_overlap: Duration::ZERO,
        observation_slack: Duration::ZERO,
    };

    /// The slack a worst-case clock *rate* error implies over one lease.
    ///
    /// Offset does not appear: both quantities are elapsed times measured by a
    /// single clock, and an offset cancels out of a difference. Rate error does
    /// not cancel, and grows with the interval being measured, which is why the
    /// lease is the scale.
    pub fn from_clock_rate_error(lease: Duration, max_rate_error: f64) -> Self {
        let error = max_rate_error.abs();
        let slack = lease.as_secs_f64() * 2.0 * error / (1.0 + error);
        let slack = Duration::from_secs_f64(slack.max(0.0));
        Self {
            belief_overlap: slack,
            observation_slack: slack,
        }
    }
}

/// The minimum a run must have achieved to count as having tested anything
///
/// Without these, a run in which every client crashed before its first claim
/// passes every safety check vacuously.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgressThresholds {
    /// Applied claims and steals
    pub min_acquisitions: usize,
    /// Applied renewals, demanded only of runs that had the chance to renew
    ///
    /// See [`progress_made`] for what "had the chance" means.
    pub min_renewals: usize,
    /// Distinct leader identities the watchers saw
    pub min_observed_identities: usize,
    /// Unknown commits the run must have resolved, one way or the other
    ///
    /// Zero for a configuration that cannot produce one. Anything above zero
    /// also turns on the anti-rot half of [`recovery_exercised`]: a run that
    /// drew the feature and never injected is a broken injector, not a lucky
    /// run.
    pub min_recoveries: usize,
    /// How long after a belief begins its first renewal comes due
    ///
    /// Must match what the driver uses, so that "this belief outlived its
    /// renewal deadline" here means the same thing it meant to the leader.
    pub renew_interval: Duration,
}

/// One entry of the recipe's own history subspace, oldest first
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    /// What the recipe recorded
    pub kind: HistoryKind,
    /// The ballot the transition produced
    pub ballot: u64,
    /// Who caused it
    pub leader_id: String,
}

/// The transitions the recipe records; renewals are deliberately not among them
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HistoryKind {
    /// Took an absent or vacant record
    Claim,
    /// Took a record from a holder
    Steal,
    /// Gave the term up
    Resign,
}

impl HistoryKind {
    fn from_transition(kind: TransitionKind) -> Option<Self> {
        match kind {
            TransitionKind::Claim => Some(Self::Claim),
            TransitionKind::Steal => Some(Self::Steal),
            TransitionKind::Resign => Some(Self::Resign),
            TransitionKind::Renew => None,
        }
    }
}

// ============================================================================
// 1. DUAL PATH REPLAY
// ============================================================================

/// Replaying the log must reproduce the database exactly.
///
/// The two paths are independent: one is what the clients recorded they did,
/// the other is what the database ended up holding. If a write happened that
/// nobody logged, or a logged write never landed, they part company here, and
/// every other invariant in this module is reasoning about a fiction.
pub fn dual_path_replay(replay: &Replay, snapshot: Option<&ExpectedRecord>) -> InvariantReport {
    let mut violations: Vec<Violation> = replay
        .anomalies
        .iter()
        .map(|anomaly| Violation::at(anomaly.index, anomaly.detail.clone()))
        .collect();

    let last = replay.transitions.last().map(|t| t.index);
    match (replay.final_state.as_ref(), snapshot) {
        (None, None) => {}
        (Some(expected), Some(actual)) if expected == actual => {}
        (expected, actual) => {
            let detail = format!("replay says {expected:?}, the database holds {actual:?}");
            violations.push(match last {
                Some(index) => Violation::at(index, detail),
                None => Violation::global(detail),
            });
        }
    }

    InvariantReport::new("DualPathReplay", violations)
}

// ============================================================================
// 2. BALLOT SUCCESSION
// ============================================================================

/// Every applied write moves the identity by exactly the step its kind allows.
///
/// Acquisitions land at `previous + 1` unconditionally: the ballot is the
/// fencing token, so a reset would silently invalidate every rank already
/// handed out. Renewals hold the ballot and add one generation. Each write must
/// also have read the state its predecessor left, which is what makes the
/// compare-and-set a compare-and-set.
pub fn ballot_succession(replay: &Replay) -> InvariantReport {
    let mut violations = Vec::new();

    for transition in &replay.transitions {
        let previous = replay.states_before[transition.index].as_ref();

        if let Some(observed) = transition.observed {
            let expected = previous.map(ExpectedRecord::identity);
            if expected != Some(observed) {
                violations.push(Violation::at(
                    transition.index,
                    format!(
                        "read {observed:?} but its predecessor left {expected:?}: \
                         the write was decided on a stale read"
                    ),
                ));
            }
        }

        match transition.kind {
            TransitionKind::Claim | TransitionKind::Steal => {
                let expected_ballot = previous.map_or(0, |p| p.ballot) + 1;
                if transition.ballot != expected_ballot {
                    violations.push(Violation::at(
                        transition.index,
                        format!(
                            "took ballot {} where the predecessor demands {expected_ballot}: \
                             ballots never reset and never skip",
                            transition.ballot
                        ),
                    ));
                }
                let expected_generation = previous.map_or(0, |p| p.generation);
                if transition.generation != expected_generation {
                    violations.push(Violation::at(
                        transition.index,
                        format!(
                            "a new term continues the generation counter at \
                             {expected_generation}, not {}",
                            transition.generation
                        ),
                    ));
                }
            }
            TransitionKind::Renew => match previous {
                None => violations.push(Violation::at(
                    transition.index,
                    "renewed a record that does not exist",
                )),
                Some(previous) => {
                    if previous.is_vacant() {
                        violations.push(Violation::at(transition.index, "renewed a resigned term"));
                    }
                    if transition.ballot != previous.ballot {
                        violations.push(Violation::at(
                            transition.index,
                            format!(
                                "a renewal keeps ballot {}, it does not move to {}",
                                previous.ballot, transition.ballot
                            ),
                        ));
                    }
                    if transition.generation != previous.generation + 1 {
                        violations.push(Violation::at(
                            transition.index,
                            format!(
                                "a renewal adds exactly one generation to {}, not {}",
                                previous.generation, transition.generation
                            ),
                        ));
                    }
                    if transition.token != previous.token
                        || transition.leader_id != previous.leader_id
                    {
                        violations.push(Violation::at(
                            transition.index,
                            "a renewal came from something other than the holder",
                        ));
                    }
                }
            },
            TransitionKind::Resign => match previous {
                None => violations.push(Violation::at(
                    transition.index,
                    "resigned a record that does not exist",
                )),
                Some(previous) => {
                    if transition.ballot != previous.ballot
                        || transition.generation != previous.generation
                    {
                        violations.push(Violation::at(
                            transition.index,
                            format!(
                                "a resign preserves ({}, {}), it wrote ({}, {})",
                                previous.ballot,
                                previous.generation,
                                transition.ballot,
                                transition.generation
                            ),
                        ));
                    }
                }
            },
        }
    }

    InvariantReport::new("BallotSuccession", violations)
}

// ============================================================================
// 3. ONE CLAIM PER BALLOT
// ============================================================================

/// A ballot names one term, held by one process under one token.
///
/// Two applied acquisitions at the same ballot would mean two processes hold
/// ranks that neither dominates, which is precisely the state the fencing
/// composition assumes cannot exist.
pub fn one_claim_per_ballot(replay: &Replay) -> InvariantReport {
    let mut violations = Vec::new();
    let mut seen: HashMap<u64, (usize, [u8; 16])> = HashMap::new();

    for transition in &replay.transitions {
        if !transition.kind.is_acquisition() {
            continue;
        }
        match seen.get(&transition.ballot) {
            Some(&(first, token)) => violations.push(Violation::spanning(
                vec![first, transition.index],
                format!(
                    "ballot {} was acquired twice, under tokens {token:?} and {:?}",
                    transition.ballot, transition.token
                ),
            )),
            None => {
                seen.insert(transition.ballot, (transition.index, transition.token));
            }
        }
    }

    InvariantReport::new("OneClaimPerBallot", violations)
}

// ============================================================================
// 4. NO BELIEF OVERLAP
// ============================================================================

/// No two processes believe they lead at the same time.
///
/// This is the weakest of the three safety levels and the only one that rests
/// on an assumption about clocks, so it is the one stated with a tolerance. A
/// client that was killed never reports the end of its belief; it is held to
/// the horizon it had computed for itself, which is the same bound its own
/// hard-stop would have enforced had it lived.
pub fn no_belief_overlap(replay: &Replay, tolerances: &Tolerances) -> InvariantReport {
    // Every belief against every later one, so a run that produced a lot of
    // them can produce a lot of violations: bounded, and see [`Violations`] for
    // why bounding costs the judgement nothing.
    let mut violations = Violations::new();
    let epsilon = tolerances.belief_overlap.as_nanos() as u64;

    for (i, first) in replay.beliefs.iter().enumerate() {
        for second in replay.beliefs.iter().skip(i + 1) {
            let (early, late) = if first.begin_sim_nanos <= second.begin_sim_nanos {
                (first, second)
            } else {
                (second, first)
            };
            let overlap = early
                .effective_end_sim_nanos()
                .saturating_sub(late.begin_sim_nanos);
            if overlap > epsilon {
                violations.push(Violation::spanning(
                    vec![early.begin_index, late.begin_index],
                    format!(
                        "client {} believed it held ballot {} until {} ns, \
                         while client {} started believing it held ballot {} at {} ns \
                         ({overlap} ns of overlap, tolerance {epsilon} ns)",
                        early.client_id,
                        early.ballot,
                        early.effective_end_sim_nanos(),
                        late.client_id,
                        late.ballot,
                        late.begin_sim_nanos,
                    ),
                ));
            }
        }
    }

    violations.into_report("NoBeliefOverlap")
}

// ============================================================================
// 5. STEAL OBSERVATION DISCIPLINE
// ============================================================================

/// A steal is earned by watching, not by guessing.
///
/// The taker must have seen the same `(ballot, generation)` for at least the
/// lease that record advertised, on its own clock, and nothing may have changed
/// the leader record inside that window. Only leader-identity-changing commits
/// count: fenced writes, denials and observations by other clients are not
/// interference.
pub fn steal_observation_discipline(
    entries: &[LogEntry],
    replay: &Replay,
    tolerances: &Tolerances,
) -> InvariantReport {
    // Bounded: one violation per steal, and each of them can name every write
    // that landed inside the window it took. Both counts grow with the log.
    let mut violations = Violations::new();
    let slack = tolerances.observation_slack.as_nanos() as u64;

    for transition in &replay.transitions {
        if transition.kind != TransitionKind::Steal {
            continue;
        }
        let index = transition.index;
        let record = &entries[index].record;

        let observed = match transition.observed {
            Some(observed) => observed,
            None => {
                violations.push(Violation::at(
                    index,
                    "stole a term without recording what it observed",
                ));
                continue;
            }
        };

        let previous = match replay.states_before[index].as_ref() {
            Some(previous) => previous,
            None => {
                violations.push(Violation::at(index, "stole a term from an absent record"));
                continue;
            }
        };

        match record.observation_start_nanos {
            None => violations.push(Violation::at(
                index,
                "stole a term without an observation window",
            )),
            Some(start) => {
                let elapsed = record.local_nanos.saturating_sub(start);
                let required = previous.lease_nanos;
                if elapsed + slack < required {
                    violations.push(Violation::at(
                        index,
                        format!(
                            "observed the record for {elapsed} ns before taking it, \
                             but it advertised a lease of {required} ns \
                             (tolerance {slack} ns)"
                        ),
                    ));
                }
            }
        }

        // The window has to have been uninterrupted. Anything that changed the
        // leader record between the write that produced the observed identity
        // and the steal means the taker was timing a record that had already
        // moved on.
        match replay.transition_producing(observed, index) {
            None => violations.push(Violation::at(
                index,
                format!("observed {observed:?}, which no applied write ever produced"),
            )),
            Some(source) => {
                let interference: Vec<usize> = replay
                    .transitions
                    .iter()
                    .filter(|t| t.index > source.index && t.index < index)
                    .map(|t| t.index)
                    .collect();
                if !interference.is_empty() {
                    let mut indices = vec![source.index];
                    indices.extend(interference.iter().copied());
                    indices.push(index);
                    violations.push(Violation::spanning(
                        // The count in the message is the whole of it; the
                        // indices are the first few, which is what a reader
                        // needs to find the window in the log.
                        Violations::indices(indices),
                        format!(
                            "{} applied write(s) changed the leader record inside the \
                             observation window",
                            interference.len()
                        ),
                    ));
                }
            }
        }
    }

    violations.into_report("StealObservationDiscipline")
}

// ============================================================================
// 6. VACANT RECLAIM
// ============================================================================

/// An orderly handover costs nothing; a crash costs a full lease.
///
/// A resign writes a vacant record that keeps the ballot, so the successor
/// lands at `ballot + 1` immediately. The asymmetry is deliberate, and it is
/// only sound if the vacancy is genuine: a record that is merely stale must go
/// through the observation window instead.
pub fn vacant_reclaim(replay: &Replay) -> InvariantReport {
    let mut violations = Vec::new();

    for transition in &replay.transitions {
        let previous = replay.states_before[transition.index].as_ref();
        match transition.kind {
            TransitionKind::Resign => match previous {
                None => violations.push(Violation::at(
                    transition.index,
                    "resigned a record that does not exist",
                )),
                Some(previous) => {
                    if previous.is_vacant() {
                        violations.push(Violation::at(
                            transition.index,
                            "resigned an already vacant term",
                        ));
                    }
                    if transition.ballot != previous.ballot {
                        violations.push(Violation::at(
                            transition.index,
                            format!(
                                "a resign preserves ballot {}, it wrote {}: \
                                 the successor would reuse a ballot",
                                previous.ballot, transition.ballot
                            ),
                        ));
                    }
                    // Replay writes the vacancy sentinel for a resign, so the
                    // state after it is the thing to check.
                    let after = replay.states_before[transition.index + 1..]
                        .iter()
                        .flatten()
                        .next();
                    if let Some(after) = after {
                        if after.ballot == transition.ballot && !after.is_vacant() {
                            violations.push(Violation::at(
                                transition.index,
                                "a resign must leave the record vacant",
                            ));
                        }
                    }
                }
            },
            TransitionKind::Claim => {
                if let Some(previous) = previous {
                    if !previous.is_vacant() {
                        violations.push(Violation::at(
                            transition.index,
                            "claimed a held record without the observation window a steal owes",
                        ));
                    }
                }
            }
            TransitionKind::Steal => match previous {
                None => violations.push(Violation::at(
                    transition.index,
                    "stole from an absent record, which is a claim",
                )),
                Some(previous) => {
                    if previous.is_vacant() {
                        violations.push(Violation::at(
                            transition.index,
                            "stole a vacant record, which is reclaimed without waiting",
                        ));
                    }
                }
            },
            TransitionKind::Renew => {}
        }
    }

    InvariantReport::new("VacantReclaim", violations)
}

// ============================================================================
// 7. FENCING HOLDS
// ============================================================================

/// A fenced write lands only under the ballot of the term that authorized it.
///
/// This is the Kleppmann pause: a leader that stalls long enough to lose its
/// term must not be able to complete work it started before the pause. Every
/// applied fenced write must fall inside the leadership interval of the client
/// that made it, at that client's own ballot.
pub fn fencing_holds(entries: &[LogEntry], replay: &Replay) -> InvariantReport {
    let mut violations = Vec::new();

    for (index, entry) in entries.iter().enumerate() {
        if entry.record.op != OpKind::FencedWrite || !entry.record.outcome.is_applied() {
            continue;
        }
        let ballot = entry.record.ballot;
        let current = replay.states_before[index]
            .as_ref()
            .map_or(0, |state| state.ballot);

        if ballot < current {
            violations.push(Violation::at(
                index,
                format!(
                    "a write fenced at ballot {ballot} landed while the term had \
                     already moved to {current}"
                ),
            ));
            continue;
        }

        match replay.term_at(index) {
            None => violations.push(Violation::at(
                index,
                format!("a write fenced at ballot {ballot} landed while nobody held the term"),
            )),
            Some(term) => {
                if term.ballot != ballot || term.client_id != entry.client_id {
                    violations.push(Violation::at(
                        index,
                        format!(
                            "client {} wrote at ballot {ballot} while client {} held ballot {}",
                            entry.client_id, term.client_id, term.ballot
                        ),
                    ));
                }
            }
        }
    }

    InvariantReport::new("FencingHolds", violations)
}

// ============================================================================
// 8. SLEEPER WAS FENCED
// ============================================================================

/// The Kleppmann scenario actually ran, and both stale operations were refused.
///
/// [`fencing_holds`] is the safety property, and it holds vacuously over a run
/// in which the paused leader never tried anything: no applied write at a stale
/// ballot exists because no write at a stale ballot exists at all. That is the
/// difference between "the fence refused the stale writes" and "there were no
/// stale writes to refuse", and only the first is evidence.
///
/// So this is the liveness half, and it is deliberately narrow. The Sleeper
/// writes an [`OpKind::SleeperWoke`] marker once its barrier is met, which is
/// the moment the two stale operations become owed: a successor holds the term
/// and has already committed a write under a higher rank. Every marker in the
/// log must therefore be followed, by the same client and at the same ballot,
/// by a rejected fenced write and a rejected renewal.
///
/// # What it refuses to demand
///
/// A run with no Sleeper, or one whose Sleeper was killed before its barrier,
/// writes no marker and is judged on nothing: attrition is a scenario that did
/// not happen, not one that failed. Only the window between the marker and the
/// two operations is held against a client, which is one transaction wide.
pub fn sleeper_was_fenced(entries: &[LogEntry]) -> InvariantReport {
    let mut violations = Vec::new();

    for (index, entry) in entries.iter().enumerate() {
        if entry.record.op != OpKind::SleeperWoke {
            continue;
        }
        let client_id = entry.client_id;
        let ballot = entry.record.ballot;

        // Only what this client did afterwards counts: the stale operations
        // follow the marker, and an earlier write at the same ballot belongs to
        // the term while it was still live.
        let after = entries
            .iter()
            .skip(index + 1)
            .filter(|later| later.client_id == client_id && later.record.ballot == ballot);
        let mut stale_write = None;
        let mut stale_renewal = None;
        for later in after {
            match later.record.op {
                OpKind::FencedWrite => stale_write = stale_write.or(Some(later.record.outcome)),
                OpKind::Renew => stale_renewal = stale_renewal.or(Some(later.record.outcome)),
                _ => {}
            }
        }

        for (what, outcome) in [("fenced write", stale_write), ("renewal", stale_renewal)] {
            match outcome {
                None => violations.push(Violation::at(
                    index,
                    format!(
                        "client {client_id} passed its pause barrier at ballot {ballot} \
                         but never attempted the stale {what}: the scenario proves nothing \
                         if the paused leader never acts"
                    ),
                )),
                Some(outcome) if outcome.is_applied() => violations.push(Violation::at(
                    index,
                    format!(
                        "client {client_id} paused past its lease and its stale {what} \
                         at ballot {ballot} was applied rather than refused"
                    ),
                )),
                Some(_) => {}
            }
        }
    }

    InvariantReport::new("SleeperWasFenced", violations)
}

// ============================================================================
// 9. UUID RECOVERY NO DUP
// ============================================================================

/// One token accounts for at most one applied claim, and a spent token is spent.
///
/// A campaign whose commit reply was lost retries, sees its own record, and
/// must adopt it rather than write again. If it wrote again it would consume a
/// second ballot for a term it already held, and the log would show the same
/// token twice.
///
/// The other resolution is terminal. A retry that finds a *stranger* at or past
/// the ballot it wrote cannot tell whether its own write briefly landed, so the
/// attempt is retired: it must have written nothing, must not report itself as
/// an adoption, and its token must never appear again under any acquisition.
/// A token that came back after being retired would be a campaign acting on a
/// term the log has already accounted to somebody else.
pub fn uuid_recovery_no_dup(entries: &[LogEntry], replay: &Replay) -> InvariantReport {
    let mut violations = Vec::new();
    let mut seen: HashMap<(i32, [u8; 16]), (usize, u64)> = HashMap::new();

    for transition in &replay.transitions {
        if !transition.kind.is_acquisition() {
            continue;
        }
        let key = (transition.client_id, transition.token);
        match seen.get(&key) {
            Some(&(first, ballot)) => violations.push(Violation::spanning(
                vec![first, transition.index],
                format!(
                    "client {} claimed twice under one token (ballots {ballot} and {})",
                    transition.client_id, transition.ballot
                ),
            )),
            None => {
                seen.insert(key, (transition.index, transition.ballot));
            }
        }
    }

    // Where each client's tokens were retired, so that anything claiming under
    // one afterwards can be named.
    let mut retired: HashMap<(i32, [u8; 16]), usize> = HashMap::new();

    for (index, entry) in entries.iter().enumerate() {
        let record = &entry.record;
        let key = (entry.client_id, record.token);

        if record.superseded {
            if record.leader_record_written {
                violations.push(Violation::at(
                    index,
                    "a superseded attempt wrote the leader record: a retirement is terminal, \
                     it is not a write",
                ));
            }
            if record.recovery_noop {
                violations.push(Violation::at(
                    index,
                    "an attempt reports being both adopted and superseded: the two \
                     resolutions are exclusive",
                ));
            }
            if record.outcome.is_applied() {
                violations.push(Violation::at(
                    index,
                    "a superseded attempt reports success: a retired attempt wins nothing",
                ));
            }
            retired.entry(key).or_insert(index);
            continue;
        }

        if record.recovery_noop {
            if record.leader_record_written {
                violations.push(Violation::at(
                    index,
                    "a recovered unknown commit wrote a second time instead of adopting its record",
                ));
            }
            if !record.outcome.is_applied() {
                violations.push(Violation::at(
                    index,
                    "a recovery no-op reports a rejection: recovery either adopts or is superseded",
                ));
            }
        }

        let acquired = (matches!(record.op, OpKind::Claim | OpKind::Steal)
            && record.outcome.is_applied())
            || record.recovery_noop;
        if let Some(&first) = retired.get(&key) {
            if acquired {
                violations.push(Violation::spanning(
                    vec![first, index],
                    format!(
                        "client {} campaigned again under a token retired at entry {first}: \
                         a superseded attempt is spent, and a fresh campaign owes a fresh token",
                        entry.client_id
                    ),
                ));
            }
        }
    }

    InvariantReport::new("UuidRecoveryNoDup", violations)
}

// ============================================================================
// 9. PROGRESS MADE
// ============================================================================

/// The run actually elected somebody.
///
/// Safety invariants are all vacuously true of a log in which nothing happened,
/// and a workload that silently stopped doing anything (a configuration typo, a
/// deadlock, an exception swallowed in a role loop) would otherwise report a
/// clean run forever.
///
/// # The renewal floor is conditional on opportunity
///
/// Acquisitions and sightings are demanded unconditionally, renewals are not.
/// A hostile configuration can produce an honest run with many acquisitions and
/// no renewals at all: when the cluster spends the window in recovery, a claim
/// takes most of its lease just to commit, so the belief it yields is nearly
/// over before it starts and the leader is stolen from long before its first
/// renewal comes due. Failing that run would be reporting the cluster's
/// behaviour as a defect of the recipe.
///
/// So the floor is applied only when the opportunity existed: count the belief
/// intervals that outlived `renew_interval` (using the bounded end, so a leader
/// killed without reporting one is held to the horizon it had computed). If
/// none did, no renewal is required. If any did, the full `min_renewals` is
/// required, not a prorated version of it: one leader that lived long enough to
/// renew and did not is already a bug, and scaling the floor would only make
/// the check harder to reason about.
pub fn progress_made(
    entries: &[LogEntry],
    replay: &Replay,
    thresholds: &ProgressThresholds,
) -> InvariantReport {
    let mut violations = Vec::new();

    let acquisitions = replay
        .transitions
        .iter()
        .filter(|t| t.kind.is_acquisition())
        .count();
    if acquisitions < thresholds.min_acquisitions {
        violations.push(Violation::global(format!(
            "{acquisitions} applied claims and steals, expected at least {}",
            thresholds.min_acquisitions
        )));
    }

    let renewals = replay
        .transitions
        .iter()
        .filter(|t| t.kind == TransitionKind::Renew)
        .count();
    let renew_interval = thresholds.renew_interval.as_nanos() as u64;
    let opportunities = replay
        .beliefs
        .iter()
        .filter(|belief| {
            belief
                .effective_end_sim_nanos()
                .saturating_sub(belief.begin_sim_nanos)
                > renew_interval
        })
        .count();
    if opportunities > 0 && renewals < thresholds.min_renewals {
        violations.push(Violation::global(format!(
            "{renewals} applied renewals, expected at least {}: {opportunities} belief \
             interval(s) outlived their renewal deadline, {renew_interval} ns after the \
             belief began",
            thresholds.min_renewals
        )));
    }

    let mut identities: Vec<_> = entries
        .iter()
        .filter(|entry| entry.record.op == OpKind::Observe)
        .filter_map(|entry| entry.record.observed)
        .map(|observed| (observed.ballot, observed.generation, observed.vacant))
        .collect();
    identities.sort_unstable();
    identities.dedup();
    if identities.len() < thresholds.min_observed_identities {
        violations.push(Violation::global(format!(
            "the watchers saw {} distinct leader identities, expected at least {}",
            identities.len(),
            thresholds.min_observed_identities
        )));
    }

    InvariantReport::new("ProgressMade", violations)
}

// ============================================================================
// 10. HISTORY FAITHFUL
// ============================================================================

/// The recipe's own audit trail agrees with what happened.
///
/// History entries are written in the same transaction as the transition they
/// describe, so they commit together or not at all, and they are keyed by
/// commit versionstamp. Retention trimming may drop the oldest entries, so the
/// trail is checked as a suffix of the replayed transitions: anything else
/// (a gap in the middle, a wrong ballot, an entry for a transition that never
/// happened) means a history write escaped its transaction.
pub fn history_faithful(replay: &Replay, history: &[HistoryEntry]) -> InvariantReport {
    let mut violations = Vec::new();

    let expected: Vec<(HistoryKind, u64, &str, usize)> = replay
        .transitions
        .iter()
        .filter_map(|t| {
            HistoryKind::from_transition(t.kind)
                .map(|kind| (kind, t.ballot, t.leader_id.as_str(), t.index))
        })
        .collect();

    if history.len() > expected.len() {
        violations.push(Violation::global(format!(
            "the history holds {} transitions but only {} were applied",
            history.len(),
            expected.len()
        )));
        return InvariantReport::new("HistoryFaithful", violations);
    }

    // Retention trims from the front, so the trail must line up with the tail.
    let offset = expected.len() - history.len();
    for (position, actual) in history.iter().enumerate() {
        let (kind, ballot, leader_id, index) = expected[offset + position];
        if actual.kind != kind || actual.ballot != ballot || actual.leader_id != leader_id {
            violations.push(Violation::at(
                index,
                format!(
                    "the history records {:?} at ballot {} by {}, but the transition was \
                     {kind:?} at ballot {ballot} by {leader_id}",
                    actual.kind, actual.ballot, actual.leader_id
                ),
            ));
        }
    }

    InvariantReport::new("HistoryFaithful", violations)
}

// ============================================================================
// 11. RECOVERY EXERCISED
// ============================================================================

/// The unknown-commit path was actually taken, and every injection ended.
///
/// `UuidRecoveryNoDup` holds vacuously over a run in which no commit reply was
/// ever lost, and under simulation that is very nearly every run: each logged
/// transaction sets `AutomaticIdempotency`, so the client resolves the unknown
/// commit itself and the recipe's recovery is never asked anything. A run of
/// zeroes there says the invariant was never put to the question, which is the
/// same silence the suite this replaces mistook for success.
///
/// So a configuration that drew the forced-recovery feature has to show two
/// things:
///
/// - that the injector still works. At least one contender won a term, so at
///   least one reply was there to be thrown away; zero markers means the
///   injection stopped happening, and every later run of this configuration
///   would pass while testing nothing.
/// - that what it injected was resolved. A resolution is either an adoption
///   (the re-probe found its own record) or a retirement (it found a stranger
///   at or past its ballot). Natural recoveries count too: the point is that
///   the path ran, not who provoked it.
///
/// An injection still outstanding when the run ended is not a resolution, and
/// is deliberately not excused. The driver only drops a reply while the run has
/// room to re-probe it, so an unresolved one means the resumption path itself
/// stopped working.
pub fn recovery_exercised(
    entries: &[LogEntry],
    replay: &Replay,
    thresholds: &ProgressThresholds,
) -> InvariantReport {
    let mut violations = Vec::new();

    if thresholds.min_recoveries == 0 {
        return InvariantReport::new("RecoveryExercised", violations);
    }

    let markers = entries
        .iter()
        .filter(|entry| entry.record.op == OpKind::InjectedUnknown)
        .count();
    let resolutions = entries
        .iter()
        .filter(|entry| is_resolution(&entry.record))
        .count();
    let acquisitions = replay
        .transitions
        .iter()
        .filter(|transition| transition.kind.is_acquisition())
        .count();

    if markers == 0 {
        if acquisitions > 0 {
            violations.push(Violation::global(format!(
                "{acquisitions} term(s) were won and not one reply was thrown away: \
                 this configuration drew forced recovery, so the injection is broken \
                 rather than unlucky"
            )));
        }
    } else if resolutions < thresholds.min_recoveries {
        violations.push(Violation::global(format!(
            "{markers} injected unknown commit(s) produced {resolutions} resolution(s), \
             expected at least {}: an unknown commit is resolved by adopting its own \
             record or by retiring the attempt, and one that did neither was never \
             re-probed",
            thresholds.min_recoveries
        )));
    }

    InvariantReport::new("RecoveryExercised", violations)
}

/// Whether this record is one of the two ways an unknown commit ends
///
/// Public because the check phase reports the count alongside the invariant:
/// a threshold nobody can see the distance to is a threshold nobody can set.
pub fn is_resolution(record: &LogRecord) -> bool {
    record.recovery_noop
        || (record.superseded && matches!(record.op, OpKind::Claim | OpKind::Steal))
}

// ============================================================================
// ALL OF THEM
// ============================================================================

/// Everything a check phase needs to judge a run
#[derive(Debug, Clone)]
pub struct CheckInputs<'a> {
    /// The log, in commit order
    pub entries: &'a [LogEntry],
    /// What replaying it produced
    pub replay: &'a Replay,
    /// The leader record the database actually holds
    pub snapshot: Option<&'a ExpectedRecord>,
    /// The recipe's own history subspace, oldest first
    pub history: &'a [HistoryEntry],
    /// What the configuration's clock assumptions allow
    pub tolerances: Tolerances,
    /// What the configuration expects the run to have achieved
    pub thresholds: ProgressThresholds,
}

/// Run every invariant, in the order they appear in this module
pub fn check_all(inputs: &CheckInputs<'_>) -> Vec<InvariantReport> {
    vec![
        dual_path_replay(inputs.replay, inputs.snapshot),
        ballot_succession(inputs.replay),
        one_claim_per_ballot(inputs.replay),
        no_belief_overlap(inputs.replay, &inputs.tolerances),
        steal_observation_discipline(inputs.entries, inputs.replay, &inputs.tolerances),
        vacant_reclaim(inputs.replay),
        fencing_holds(inputs.entries, inputs.replay),
        sleeper_was_fenced(inputs.entries),
        uuid_recovery_no_dup(inputs.entries, inputs.replay),
        progress_made(inputs.entries, inputs.replay, &inputs.thresholds),
        history_faithful(inputs.replay, inputs.history),
        recovery_exercised(inputs.entries, inputs.replay, &inputs.thresholds),
    ]
}

#[cfg(test)]
mod tests {
    use super::super::log_schema::fixtures::*;
    use super::super::log_schema::{LogEntry, OpKind, Outcome};
    use super::super::replay::replay;
    use super::*;

    const THRESHOLDS: ProgressThresholds = ProgressThresholds {
        min_acquisitions: 3,
        min_renewals: 3,
        min_observed_identities: 4,
        renew_interval: Duration::from_nanos(LEASE / 3),
        // The clean log predates forced recovery: it resolves an unknown commit
        // naturally, and demanding one of it would be demanding a coincidence.
        min_recoveries: 0,
    };

    /// What a run that drew the forced-recovery feature has to show
    const RECOVERY_THRESHOLDS: ProgressThresholds = ProgressThresholds {
        min_acquisitions: 4,
        min_renewals: 2,
        min_observed_identities: 3,
        renew_interval: Duration::from_nanos(LEASE / 3),
        min_recoveries: 1,
    };

    /// Replay a log the way the check phase does
    fn replayed(entries: &[LogEntry]) -> Replay {
        replay(entries, leader_id)
    }

    fn inputs<'a>(
        entries: &'a [LogEntry],
        out: &'a Replay,
        snapshot: &'a ExpectedRecord,
        history: &'a [HistoryEntry],
        thresholds: ProgressThresholds,
    ) -> CheckInputs<'a> {
        CheckInputs {
            entries,
            replay: out,
            snapshot: Some(snapshot),
            history,
            tolerances: Tolerances::STRICT,
            thresholds,
        }
    }

    /// Assert the invariant failed, *and* that it failed for the reason the
    /// test set out to provoke.
    ///
    /// Without the second half, a mutation that happens to trip some unrelated
    /// check would look like a passing falsification test, and the invariant
    /// under test could quietly become a tautology again.
    fn assert_failed(report: &InvariantReport, because: &str) {
        assert!(
            !report.passed(),
            "{} passed on a log built to break it",
            report.name
        );
        assert!(
            report
                .violations
                .iter()
                .any(|violation| violation.detail.contains(because)),
            "{} failed, but not for the reason under test ({because:?}): {:?}",
            report.name,
            report.violations
        );
    }

    fn find(entries: &[LogEntry], predicate: impl Fn(&LogEntry) -> bool) -> usize {
        entries
            .iter()
            .position(predicate)
            .expect("the fixture contains the entry this test mutates")
    }

    // ------------------------------------------------------------------
    // The clean log satisfies everything, with no slack at all.
    // ------------------------------------------------------------------

    #[test]
    fn a_well_behaved_run_satisfies_every_invariant() {
        let entries = clean_log();
        let out = replayed(&entries);
        let snapshot = clean_snapshot();
        let history = clean_history();
        for report in check_all(&inputs(&entries, &out, &snapshot, &history, THRESHOLDS)) {
            assert!(
                report.passed(),
                "{} failed on the clean log: {:?}",
                report.name,
                report.violations
            );
        }
    }

    #[test]
    fn a_run_that_forced_two_unknown_commits_satisfies_every_invariant() {
        // Both resolutions in one log: an adoption and a retirement, each
        // following an injected marker, with no slack spent anywhere.
        let entries = injected_recovery_log();
        let out = replayed(&entries);
        let snapshot = injected_recovery_snapshot();
        let history = injected_recovery_history();
        for report in check_all(&inputs(
            &entries,
            &out,
            &snapshot,
            &history,
            RECOVERY_THRESHOLDS,
        )) {
            assert!(
                report.passed(),
                "{} failed on the injected-recovery log: {:?}",
                report.name,
                report.violations
            );
        }
    }

    // ------------------------------------------------------------------
    // 1. DualPathReplay
    // ------------------------------------------------------------------

    #[test]
    fn dual_path_replay_catches_a_database_that_moved_on_its_own() {
        let entries = clean_log();
        let out = replayed(&entries);
        assert!(dual_path_replay(&out, Some(&clean_snapshot())).passed());

        // A write nobody logged: the database is one ballot ahead.
        let mut snapshot = clean_snapshot();
        snapshot.ballot += 1;
        assert_failed(
            &dual_path_replay(&out, Some(&snapshot)),
            "the database holds",
        );

        // And a logged write that never landed.
        assert_failed(&dual_path_replay(&out, None), "the database holds");
    }

    #[test]
    fn dual_path_replay_catches_a_self_contradictory_entry() {
        let mut entries = clean_log();
        let fenced = find(&entries, |entry| entry.record.op == OpKind::FencedWrite);
        entries[fenced].record.leader_record_written = true;

        let out = replayed(&entries);
        assert_failed(
            &dual_path_replay(&out, Some(&clean_snapshot())),
            "cannot write the leader record",
        );
    }

    // ------------------------------------------------------------------
    // 2. BallotSuccession
    // ------------------------------------------------------------------

    #[test]
    fn ballot_succession_catches_a_ballot_that_reset() {
        // The defect the whole rewrite exists for: leadership handed back at a
        // ballot already used, invalidating every fencing rank derived from it.
        let mut entries = clean_log();
        let reclaim = find(&entries, |entry| {
            entry.record.op == OpKind::Claim && entry.record.ballot == 2
        });
        entries[reclaim].record.ballot = 1;

        assert_failed(
            &ballot_succession(&replayed(&entries)),
            "ballots never reset and never skip",
        );
    }

    #[test]
    fn ballot_succession_catches_a_ballot_that_skipped() {
        let mut entries = clean_log();
        let steal = find(&entries, |entry| entry.record.op == OpKind::Steal);
        entries[steal].record.ballot = 5;

        assert_failed(
            &ballot_succession(&replayed(&entries)),
            "ballots never reset and never skip",
        );
    }

    #[test]
    fn ballot_succession_catches_a_renewal_that_moved_the_ballot() {
        let mut entries = clean_log();
        let renew = find(&entries, |entry| {
            entry.record.op == OpKind::Renew && entry.record.leader_record_written
        });
        entries[renew].record.ballot += 1;

        assert_failed(
            &ballot_succession(&replayed(&entries)),
            "a renewal keeps ballot",
        );
    }

    #[test]
    fn ballot_succession_catches_a_renewal_that_skipped_a_generation() {
        let mut entries = clean_log();
        let renew = find(&entries, |entry| {
            entry.record.op == OpKind::Renew && entry.record.leader_record_written
        });
        entries[renew].record.generation += 1;

        assert_failed(
            &ballot_succession(&replayed(&entries)),
            "a renewal adds exactly one generation",
        );
    }

    #[test]
    fn ballot_succession_catches_a_write_decided_on_a_stale_read() {
        let mut entries = clean_log();
        let steal = find(&entries, |entry| entry.record.op == OpKind::Steal);
        let observed = entries[steal]
            .record
            .observed
            .as_mut()
            .expect("a steal records what it read");
        observed.generation -= 1;

        assert_failed(&ballot_succession(&replayed(&entries)), "stale read");
    }

    // ------------------------------------------------------------------
    // 3. OneClaimPerBallot
    // ------------------------------------------------------------------

    #[test]
    fn one_claim_per_ballot_catches_two_holders_of_one_term() {
        let mut entries = clean_log();
        let steal = find(&entries, |entry| entry.record.op == OpKind::Steal);
        // A second process lands the same ballot: both now hold ranks that
        // neither dominates.
        let mut duplicate = entries[steal].clone();
        duplicate.client_id = 0;
        duplicate.record.token = token(7);
        duplicate.record.sim_nanos += SEC;
        entries.insert(steal + 1, duplicate);

        assert_failed(
            &one_claim_per_ballot(&replayed(&entries)),
            "was acquired twice",
        );
    }

    // ------------------------------------------------------------------
    // Bounded reporting
    // ------------------------------------------------------------------

    #[test]
    fn a_capped_report_still_fails_and_says_what_it_dropped() {
        let mut violations = Violations::new();
        for index in 0..(MAX_VIOLATIONS_KEPT * 3) {
            violations.push(Violation::at(index, "broke it"));
        }
        let report = violations.into_report("Bounded");

        assert!(!report.passed(), "a capped report must still fail the run");
        assert_eq!(
            report.violations.len(),
            MAX_VIOLATIONS_KEPT + 1,
            "everything kept, plus the one line counting the rest"
        );
        let last = report.violations.last().expect("the remainder line");
        assert!(
            last.detail
                .contains(&format!("{} were found", MAX_VIOLATIONS_KEPT * 3)),
            "the remainder must name the real total, got {:?}",
            last.detail
        );
        assert!(last.indices.is_empty(), "the remainder names no entry");
    }

    #[test]
    fn a_report_under_the_cap_is_untouched() {
        let mut violations = Violations::new();
        violations.push(Violation::at(7, "broke it"));
        let report = violations.into_report("Bounded");
        assert_eq!(report.violations, vec![Violation::at(7, "broke it")]);

        // And an invariant that held reports nothing at all, remainder line
        // included: the cap must not turn a pass into a failure.
        assert!(Violations::new().into_report("Bounded").passed());
    }

    #[test]
    fn one_violation_cannot_name_an_unbounded_number_of_entries() {
        let indices = Violations::indices((0..10_000).collect());
        assert_eq!(indices.len(), MAX_INDICES_KEPT);
        assert_eq!(indices[0], 0, "the first are the ones a reader needs");
    }

    // ------------------------------------------------------------------
    // 4. NoBeliefOverlap
    // ------------------------------------------------------------------

    #[test]
    fn no_belief_overlap_catches_two_leaders_believing_at_once() {
        let entries = clean_log();
        assert!(no_belief_overlap(&replayed(&entries), &Tolerances::STRICT).passed());

        // The successor starts believing while the predecessor still does.
        let mut entries = clean_log();
        let successor = find(&entries, |entry| {
            entry.record.op == OpKind::BeliefBegin && entry.record.ballot == 2
        });
        entries[successor].record.sim_nanos = 3 * SEC;
        entries[successor].record.local_nanos = 3 * SEC;

        assert_failed(
            &no_belief_overlap(&replayed(&entries), &Tolerances::STRICT),
            "of overlap",
        );
    }

    #[test]
    fn no_belief_overlap_holds_a_killed_leader_to_its_horizon() {
        // Client 1 is killed without ever reporting an end. Pushing its horizon
        // past the moment client 2 starts believing is exactly the failure a
        // too-generous hard-stop would produce.
        let mut entries = clean_log();
        let crashed = find(&entries, |entry| {
            entry.record.op == OpKind::BeliefBegin && entry.record.ballot == 2
        });
        entries[crashed].record.horizon_nanos = 30 * SEC;

        assert_failed(
            &no_belief_overlap(&replayed(&entries), &Tolerances::STRICT),
            "of overlap",
        );
    }

    #[test]
    fn belief_overlap_tolerance_is_only_spent_where_a_configuration_allows_it() {
        let mut entries = clean_log();
        let successor = find(&entries, |entry| {
            entry.record.op == OpKind::BeliefBegin && entry.record.ballot == 3
        });
        // A tenth of a second of overlap with the crashed leader's horizon: a
        // violation with identical clocks, inside the budget once a 5% rate
        // error is admitted over a ten second lease.
        entries[successor].record.sim_nanos = 15 * SEC + 3 * SEC / 10;
        let out = replayed(&entries);

        assert_failed(&no_belief_overlap(&out, &Tolerances::STRICT), "of overlap");
        let generous = Tolerances::from_clock_rate_error(Duration::from_nanos(LEASE), 0.05);
        assert!(no_belief_overlap(&out, &generous).passed());
    }

    // ------------------------------------------------------------------
    // 5. StealObservationDiscipline
    // ------------------------------------------------------------------

    #[test]
    fn steal_discipline_catches_a_window_shorter_than_the_lease() {
        let mut entries = clean_log();
        let steal = find(&entries, |entry| entry.record.op == OpKind::Steal);
        // Started timing five seconds late: half a lease of observation.
        entries[steal].record.observation_start_nanos = Some(13 * SEC);

        let out = replayed(&entries);
        assert_failed(
            &steal_observation_discipline(&entries, &out, &Tolerances::STRICT),
            "advertised a lease of",
        );
    }

    #[test]
    fn steal_discipline_catches_an_interrupted_window() {
        // The victim renewed inside the window: the taker was timing a record
        // that had already moved.
        let mut entries = clean_log();
        let steal = find(&entries, |entry| entry.record.op == OpKind::Steal);
        let renewal = LogEntry {
            versionstamp: entries[steal].versionstamp,
            client_id: 1,
            op_num: 999,
            record: write(OpKind::Renew, 2, 4, 2, 15 * SEC),
        };
        entries.insert(steal, renewal);
        // The taker still writes what the (now stale) record it timed implies.
        entries[steal + 1].record.generation = 4;

        let out = replayed(&entries);
        assert_failed(
            &steal_observation_discipline(&entries, &out, &Tolerances::STRICT),
            "inside the observation window",
        );
    }

    #[test]
    fn steal_discipline_catches_a_steal_with_no_window_at_all() {
        let mut entries = clean_log();
        let steal = find(&entries, |entry| entry.record.op == OpKind::Steal);
        entries[steal].record.observation_start_nanos = None;

        let out = replayed(&entries);
        assert_failed(
            &steal_observation_discipline(&entries, &out, &Tolerances::STRICT),
            "without an observation window",
        );
    }

    #[test]
    fn steal_discipline_spends_its_tolerance_on_clock_rate_error_only() {
        let mut entries = clean_log();
        let steal = find(&entries, |entry| entry.record.op == OpKind::Steal);
        // A tenth of a second short of a full lease.
        entries[steal].record.observation_start_nanos = Some(8 * SEC + 4 * SEC / 10);
        let out = replayed(&entries);

        assert_failed(
            &steal_observation_discipline(&entries, &out, &Tolerances::STRICT),
            "advertised a lease of",
        );
        let generous = Tolerances::from_clock_rate_error(Duration::from_nanos(LEASE), 0.01);
        assert!(steal_observation_discipline(&entries, &out, &generous).passed());
    }

    // ------------------------------------------------------------------
    // 6. VacantReclaim
    // ------------------------------------------------------------------

    #[test]
    fn vacant_reclaim_catches_a_resign_that_reset_the_ballot() {
        let mut entries = clean_log();
        let resign = find(&entries, |entry| entry.record.op == OpKind::Resign);
        entries[resign].record.ballot = 0;

        assert_failed(
            &vacant_reclaim(&replayed(&entries)),
            "a resign preserves ballot",
        );
    }

    #[test]
    fn vacant_reclaim_catches_a_claim_over_a_live_record() {
        // A claim skips the observation window entirely, so taking a held
        // record with one is the instant-override defect.
        let mut entries = clean_log();
        let steal = find(&entries, |entry| entry.record.op == OpKind::Steal);
        entries[steal].record.op = OpKind::Claim;

        assert_failed(
            &vacant_reclaim(&replayed(&entries)),
            "claimed a held record",
        );
    }

    #[test]
    fn vacant_reclaim_catches_a_steal_of_a_resigned_term() {
        let mut entries = clean_log();
        let reclaim = find(&entries, |entry| {
            entry.record.op == OpKind::Claim && entry.record.ballot == 2
        });
        entries[reclaim].record.op = OpKind::Steal;

        assert_failed(
            &vacant_reclaim(&replayed(&entries)),
            "stole a vacant record",
        );
    }

    // ------------------------------------------------------------------
    // 7. FencingHolds
    // ------------------------------------------------------------------

    #[test]
    fn fencing_holds_catches_the_paused_leaders_write_landing() {
        // The Kleppmann scenario, which the previous suite did not test at all:
        // client 1 pauses past its lease, wakes up, and its stale write must be
        // rejected. Flipping it to applied is the defect.
        let mut entries = clean_log();
        let stale = find(&entries, |entry| {
            entry.record.op == OpKind::FencedWrite && entry.record.outcome == Outcome::Rejected
        });
        entries[stale].record.outcome = Outcome::Applied;

        assert_failed(
            &fencing_holds(&entries, &replayed(&entries)),
            "already moved to",
        );
    }

    #[test]
    fn fencing_holds_catches_a_write_at_somebody_elses_ballot() {
        let mut entries = clean_log();
        let fenced = find(&entries, |entry| {
            entry.record.op == OpKind::FencedWrite && entry.record.outcome == Outcome::Applied
        });
        entries[fenced].client_id = 2;

        assert_failed(&fencing_holds(&entries, &replayed(&entries)), "held ballot");
    }

    // ------------------------------------------------------------------
    // 8. SleeperWasFenced
    // ------------------------------------------------------------------

    #[test]
    fn sleeper_was_fenced_catches_a_scenario_that_never_ran() {
        let entries = clean_log();
        assert!(sleeper_was_fenced(&entries).passed());

        // The defect this exists for, and the one `FencingHolds` cannot see:
        // the Sleeper passes its barrier and then never touches its stale term,
        // so there is no applied write at a stale ballot for the fencing check
        // to object to, and the run passes having proved nothing.
        let mut entries = clean_log();
        entries.retain(|entry| {
            !(entry.client_id == 1
                && entry.record.ballot == 2
                && matches!(entry.record.op, OpKind::FencedWrite | OpKind::Renew)
                && entry.record.outcome == Outcome::Rejected)
        });
        assert!(
            fencing_holds(&entries, &replayed(&entries)).passed(),
            "the safety half is exactly what stays silent here"
        );
        assert_failed(&sleeper_was_fenced(&entries), "never attempted the stale");
    }

    #[test]
    fn sleeper_was_fenced_catches_each_missing_operation_on_its_own() {
        // Only the write goes missing.
        let mut entries = clean_log();
        entries.retain(|entry| {
            !(entry.client_id == 1
                && entry.record.op == OpKind::FencedWrite
                && entry.record.outcome == Outcome::Rejected)
        });
        assert_failed(
            &sleeper_was_fenced(&entries),
            "never attempted the stale fenced write",
        );

        // Only the renewal goes missing.
        let mut entries = clean_log();
        entries.retain(|entry| {
            !(entry.client_id == 1
                && entry.record.op == OpKind::Renew
                && entry.record.outcome == Outcome::Rejected)
        });
        assert_failed(
            &sleeper_was_fenced(&entries),
            "never attempted the stale renewal",
        );
    }

    #[test]
    fn sleeper_was_fenced_catches_a_stale_operation_that_was_not_refused() {
        let mut entries = clean_log();
        let stale = find(&entries, |entry| {
            entry.client_id == 1
                && entry.record.op == OpKind::FencedWrite
                && entry.record.outcome == Outcome::Rejected
        });
        entries[stale].record.outcome = Outcome::Applied;

        assert_failed(&sleeper_was_fenced(&entries), "was applied rather than");
    }

    #[test]
    fn sleeper_was_fenced_demands_nothing_of_a_run_without_one() {
        // No marker, no demand. A run that drew no Sleeper, and one whose
        // Sleeper was killed before its barrier, both look like this, and
        // neither is a failure: attrition is a scenario that did not happen.
        let mut entries = clean_log();
        entries.retain(|entry| entry.record.op != OpKind::SleeperWoke);
        assert!(sleeper_was_fenced(&entries).passed());

        assert!(sleeper_was_fenced(&[]).passed());
    }

    #[test]
    fn sleeper_was_fenced_ignores_what_the_term_did_while_it_was_live() {
        // The stale operations follow the marker. A fenced write the Sleeper
        // made at the same ballot *before* pausing is the term acting while it
        // still held, and counting it would let a run pass on evidence from
        // before the pause.
        let mut entries = clean_log();
        let marker = find(&entries, |entry| entry.record.op == OpKind::SleeperWoke);
        let live_write = {
            let mut record = fenced_write(2, Outcome::Applied, 7 * SEC);
            record.local_nanos = 7 * SEC;
            record
        };
        entries.insert(
            marker,
            LogEntry {
                client_id: 1,
                op_num: 0,
                versionstamp: [0u8; 12],
                record: live_write,
            },
        );
        // Now remove the genuinely stale write: only the pre-pause one is left.
        entries.retain(|entry| {
            !(entry.client_id == 1
                && entry.record.op == OpKind::FencedWrite
                && entry.record.outcome == Outcome::Rejected)
        });

        assert_failed(
            &sleeper_was_fenced(&entries),
            "never attempted the stale fenced write",
        );
    }

    // ------------------------------------------------------------------
    // 9. UuidRecoveryNoDup
    // ------------------------------------------------------------------

    #[test]
    fn uuid_recovery_catches_a_recovered_claim_written_twice() {
        // The unknown-commit path: the retry must adopt the record it already
        // wrote. Writing again consumes a second ballot under one token.
        let mut entries = clean_log();
        let recovery = find(&entries, |entry| entry.record.recovery_noop);
        entries[recovery].record.leader_record_written = true;
        entries[recovery].record.recovery_noop = false;
        entries[recovery].record.ballot = 3;

        assert_failed(
            &uuid_recovery_no_dup(&entries, &replayed(&entries)),
            "claimed twice under one token",
        );
    }

    #[test]
    fn uuid_recovery_catches_a_recovery_that_wrote() {
        let mut entries = clean_log();
        let recovery = find(&entries, |entry| entry.record.recovery_noop);
        entries[recovery].record.leader_record_written = true;

        assert_failed(
            &uuid_recovery_no_dup(&entries, &replayed(&entries)),
            "wrote a second time",
        );
    }

    #[test]
    fn uuid_recovery_catches_a_retired_token_claiming_again() {
        // A retirement is terminal: the attempt may have written a record
        // somebody else now owns, so the token cannot come back. Here it comes
        // back as an adoption, which is the shape that would silently give one
        // client two accounts of the same term.
        let mut entries = injected_recovery_log();
        let adoption = find(&entries, |entry| entry.record.recovery_noop);
        let mut again = entries[adoption].clone();
        again.client_id = 1;
        again.record.token = token(2);
        again.versionstamp = [0xff; 12];
        entries.push(again);

        assert_failed(
            &uuid_recovery_no_dup(&entries, &replayed(&entries)),
            "under a token retired at entry",
        );
    }

    #[test]
    fn uuid_recovery_catches_a_superseded_entry_that_wrote() {
        let mut entries = injected_recovery_log();
        let retired = find(&entries, |entry| entry.record.superseded);
        entries[retired].record.leader_record_written = true;
        assert_failed(
            &uuid_recovery_no_dup(&entries, &replayed(&entries)),
            "a retirement is terminal",
        );

        // And a retirement that reports success: the attempt won nothing, and
        // an applied outcome here is what would make replay count it as a term.
        let mut entries = injected_recovery_log();
        entries[retired].record.outcome = Outcome::Applied;
        assert_failed(
            &uuid_recovery_no_dup(&entries, &replayed(&entries)),
            "a retired attempt wins nothing",
        );
    }

    #[test]
    fn uuid_recovery_catches_a_superseded_recovery_noop_contradiction() {
        // The two resolutions are exclusive: an attempt either found its own
        // record or found a stranger. An entry claiming both would let a run
        // pass the resolution count while describing something impossible.
        let mut entries = injected_recovery_log();
        let retired = find(&entries, |entry| entry.record.superseded);
        entries[retired].record.recovery_noop = true;

        assert_failed(
            &uuid_recovery_no_dup(&entries, &replayed(&entries)),
            "both adopted and superseded",
        );
    }

    // ------------------------------------------------------------------
    // 9. ProgressMade
    // ------------------------------------------------------------------

    #[test]
    fn progress_made_catches_a_run_where_nothing_happened() {
        let entries: Vec<LogEntry> = Vec::new();
        let out = replayed(&entries);
        assert_failed(
            &progress_made(&entries, &out, &THRESHOLDS),
            "applied claims and steals",
        );
    }

    #[test]
    fn progress_made_catches_a_run_that_only_ever_elected_once() {
        let mut log = LogBuilder::new();
        log.push(0, write(OpKind::Claim, 1, 0, 1, SEC));
        let entries = log.into_entries();
        let out = replayed(&entries);
        assert_failed(
            &progress_made(&entries, &out, &THRESHOLDS),
            "applied claims and steals",
        );
    }

    #[test]
    fn progress_made_catches_a_leader_that_lived_long_enough_to_renew_and_did_not() {
        // Every belief in the clean log outlives its renewal deadline, so the
        // floor applies in full: losing the renewals is a defect, not bad luck.
        let entries: Vec<LogEntry> = clean_log()
            .into_iter()
            .filter(|entry| entry.record.op != OpKind::Renew)
            .collect();
        let out = replayed(&entries);
        assert_failed(
            &progress_made(&entries, &out, &THRESHOLDS),
            "outlived their renewal deadline",
        );
    }

    #[test]
    fn progress_made_excuses_a_run_that_never_got_the_chance_to_renew() {
        // The hostile configurations produce honest runs like this one: the
        // cluster spends the window in recovery, each claim takes most of its
        // lease just to commit, and the belief that comes out of it is nearly
        // over before it begins. Nobody ever reaches a renewal deadline, and a
        // flat floor would fail the run for the cluster's behaviour.
        let thresholds = ProgressThresholds {
            min_acquisitions: 2,
            min_renewals: 3,
            min_observed_identities: 0,
            renew_interval: Duration::from_nanos(LEASE / 3),
            min_recoveries: 0,
        };

        let mut log = LogBuilder::new();
        log.push(0, write(OpKind::Claim, 1, 0, 1, 9 * SEC));
        // Anchored at zero, committed at nine: one second of horizon survives,
        // and the leader is killed without ever reporting an end.
        log.push(0, belief_begin(0, 1, 9 * SEC, 10 * SEC));
        let mut steal = write(OpKind::Steal, 2, 0, 2, 20 * SEC);
        steal.observation_start_nanos = Some(10 * SEC);
        observed(&mut steal, 1, 0, false);
        log.push(1, steal);
        log.push(1, belief_begin(1, 2, 20 * SEC, 21 * SEC));

        let mut entries = log.into_entries();
        let out = replayed(&entries);
        assert!(
            progress_made(&entries, &out, &thresholds).passed(),
            "a run in which nobody reached a renewal deadline must not be held to the floor"
        );

        // The condition is a condition, not an escape hatch: one belief that
        // does outlive its deadline brings the floor back.
        let belief = find(&entries, |entry| entry.record.op == OpKind::BeliefBegin);
        entries[belief].record.horizon_nanos = 19 * SEC;
        let out = replayed(&entries);
        assert_failed(
            &progress_made(&entries, &out, &thresholds),
            "outlived their renewal deadline",
        );
    }

    #[test]
    fn progress_made_catches_a_blind_watcher() {
        let entries: Vec<LogEntry> = clean_log()
            .into_iter()
            .filter(|entry| entry.record.op != OpKind::Observe)
            .collect();
        let out = replayed(&entries);
        assert_failed(
            &progress_made(&entries, &out, &THRESHOLDS),
            "distinct leader identities",
        );
    }

    // ------------------------------------------------------------------
    // 10. HistoryFaithful
    // ------------------------------------------------------------------

    #[test]
    fn history_faithful_accepts_a_trimmed_trail() {
        // Retention drops the oldest entries, so a suffix is legitimate.
        let entries = clean_log();
        let out = replayed(&entries);
        let history = clean_history();
        assert!(history_faithful(&out, &history[2..]).passed());
    }

    #[test]
    fn history_faithful_catches_a_transition_missing_from_the_middle() {
        let entries = clean_log();
        let out = replayed(&entries);
        let mut history = clean_history();
        history.remove(1);
        assert_failed(&history_faithful(&out, &history), "the history records");
    }

    #[test]
    fn history_faithful_catches_a_wrong_ballot() {
        let entries = clean_log();
        let out = replayed(&entries);
        let mut history = clean_history();
        history[3].ballot = 9;
        assert_failed(&history_faithful(&out, &history), "the history records");
    }

    #[test]
    fn history_faithful_catches_an_entry_for_a_transition_that_never_happened() {
        let entries = clean_log();
        let out = replayed(&entries);
        let mut history = clean_history();
        history.push(HistoryEntry {
            kind: HistoryKind::Steal,
            ballot: 4,
            leader_id: leader_id(0),
        });
        assert_failed(&history_faithful(&out, &history), "but only");
    }

    #[test]
    fn history_faithful_rejects_a_renewal_that_leaked_into_the_trail() {
        // Renewals are deliberately not recorded: the trail is a rare-event
        // audit trail, and logging every heartbeat would make it a contention
        // point. An extra entry shifts the suffix and is caught.
        let entries = clean_log();
        let out = replayed(&entries);
        let mut history = clean_history();
        history.insert(
            1,
            HistoryEntry {
                kind: HistoryKind::Claim,
                ballot: 1,
                leader_id: leader_id(0),
            },
        );
        assert_failed(&history_faithful(&out, &history), "but only");
    }

    // ------------------------------------------------------------------
    // 11. RecoveryExercised
    // ------------------------------------------------------------------

    #[test]
    fn recovery_exercised_catches_an_injection_that_never_resolved() {
        // The markers say two replies were thrown away and nothing in the log
        // says either attempt was ever re-probed: the resumption path stopped
        // working, and every safety check about recovery is vacuous.
        let entries: Vec<LogEntry> = injected_recovery_log()
            .into_iter()
            .filter(|entry| !is_resolution(&entry.record))
            .collect();
        let out = replayed(&entries);

        assert_failed(
            &recovery_exercised(&entries, &out, &RECOVERY_THRESHOLDS),
            "was never re-probed",
        );
    }

    #[test]
    fn recovery_exercised_catches_an_injector_that_rotted() {
        // The anti-rot half. The clean log wins terms and carries no marker at
        // all, which for a configuration that drew the feature means the
        // injection is broken rather than the run lucky. Without this the
        // feature could stop firing and every run would still pass.
        let entries = clean_log();
        let out = replayed(&entries);

        assert_failed(
            &recovery_exercised(&entries, &out, &RECOVERY_THRESHOLDS),
            "not one reply was thrown away",
        );
    }

    #[test]
    fn recovery_exercised_is_not_demanded_when_the_feature_was_not_drawn() {
        // Off means the run cannot inject, so demanding a recovery of it would
        // fail honest runs. The same log, judged by a plan that drew nothing.
        let entries = clean_log();
        let out = replayed(&entries);
        assert!(recovery_exercised(&entries, &out, &THRESHOLDS).passed());

        // And a run that did nothing at all is ProgressMade's business: with no
        // term won there was no reply to throw away.
        let empty: Vec<LogEntry> = Vec::new();
        let out = replayed(&empty);
        assert!(recovery_exercised(&empty, &out, &RECOVERY_THRESHOLDS).passed());
    }

    #[test]
    fn recovery_exercised_counts_a_natural_recovery() {
        // The point is that the path ran, not who provoked it: the clean log's
        // own lost reply is a resolution, so a run that recovered naturally and
        // also injected is not held to a second recovery.
        let mut entries = injected_recovery_log();
        let marker = find(&entries, |entry| entry.record.op == OpKind::InjectedUnknown);
        let adoption = find(&entries, |entry| entry.record.recovery_noop);
        entries.remove(adoption);
        let out = replayed(&entries);
        assert!(
            recovery_exercised(&entries, &out, &RECOVERY_THRESHOLDS).passed(),
            "the retirement alone resolves the run"
        );

        // Both markers still there, and the floor is the count of resolutions.
        assert!(entries[marker].record.op == OpKind::InjectedUnknown);
        let demanding = ProgressThresholds {
            min_recoveries: 2,
            ..RECOVERY_THRESHOLDS
        };
        assert_failed(
            &recovery_exercised(&entries, &out, &demanding),
            "expected at least 2",
        );
    }

    // ------------------------------------------------------------------
    // Tolerances
    // ------------------------------------------------------------------

    #[test]
    fn the_strict_configuration_admits_no_slack_at_all() {
        assert_eq!(Tolerances::STRICT.belief_overlap, Duration::ZERO);
        assert_eq!(
            Tolerances::from_clock_rate_error(Duration::from_secs(10), 0.0),
            Tolerances::STRICT
        );
        // 0.1% of rate error over ten seconds is just under twenty milliseconds.
        let derived = Tolerances::from_clock_rate_error(Duration::from_secs(10), 1e-3);
        assert!(derived.observation_slack > Duration::from_millis(19));
        assert!(derived.observation_slack < Duration::from_millis(20));
    }
}
