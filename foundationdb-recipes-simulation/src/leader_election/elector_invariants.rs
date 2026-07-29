//! The six properties a run of the real elector has to satisfy.
//!
//! The driver's election is judged from a log that wraps every protocol step,
//! so [`invariants`](super::invariants) can reason about what each transaction
//! decided. The elector's election cannot be judged that way: the recipe owns
//! its transactions, and instrumenting them would mean judging a copy of the
//! elector rather than the elector. So the evidence here is what the run left
//! behind, from two independent places:
//!
//! - the recipe's own history subspace, which it writes in the same transaction
//!   as the transition it describes, keyed by commit versionstamp;
//! - the elector role's log ([`elector_role`](super::elector_role)), which holds
//!   the beliefs and the fenced writes, keyed the same way.
//!
//! Neither side can forge the other's entries, and the ten-byte commit version
//! puts both into the one order FoundationDB actually committed them in. That
//! merged order is what every check below reads, which is what makes them
//! checks about effects: not "did the elector take the right code path" but
//! "did a write land outside the term that authorized it".
//!
//! As in the sibling module, a check earns its place only with a
//! counterexample, and every counterexample is a test.

use std::collections::HashMap;

use super::invariants::{HistoryKind, InvariantReport, Tolerances, Violation};
use super::log_schema::{LogEntry, OpKind};
use super::replay::Replay;

// ============================================================================
// EVIDENCE
// ============================================================================

/// One entry of the elector election's history, with the version that orders it
///
/// The recipe's [`HistoryEvent`] carries a full twelve-byte versionstamp; only
/// the ten-byte commit version is comparable against another transaction's, so
/// that is what is kept.
///
/// [`HistoryEvent`]: foundationdb::recipes::leader_election::HistoryEvent
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StampedTransition {
    /// Commit version of the transaction that wrote it
    pub stamp: [u8; 10],
    /// What the recipe recorded
    pub kind: HistoryKind,
    /// The ballot the transition produced
    pub ballot: u64,
    /// Who caused it
    pub leader_id: String,
}

/// The leader record the elector election ended up holding
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElectorSnapshot {
    /// Term number
    pub ballot: u64,
    /// Holder, empty when vacant
    pub leader_id: String,
    /// Whether the term was resigned rather than held
    pub vacant: bool,
}

/// What a run that ran real electors has to have achieved
///
/// Without it, a run in which both electors failed to build passes every safety
/// check by never doing anything, which is the silence this whole suite exists
/// to refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ElectorThresholds {
    /// Terms the electors must have won between them
    pub min_acquisitions: usize,
    /// Fenced writes that must have been applied under those terms
    pub min_fenced_writes: usize,
}

impl ElectorThresholds {
    /// What is demanded of a run whose plan drew the feature and whose field
    /// could spare the clients
    ///
    /// One of each, and no more. Two electors on a short lease produce dozens
    /// of both, but a run whose cluster spent its window in recovery may manage
    /// exactly one term, and failing that run would report the cluster's
    /// behaviour as a defect of the recipe. What the floor rules out is the run
    /// that produced none at all, which is the only outcome that makes the
    /// safety checks vacuous.
    pub const ACTIVE: Self = Self {
        min_acquisitions: 1,
        min_fenced_writes: 1,
    };
}

/// Everything the elector half of the check phase judges from
#[derive(Debug, Clone)]
pub struct ElectorEvidence<'a> {
    /// The elector election's history, oldest first
    pub history: &'a [StampedTransition],
    /// The elector role's log, in commit order
    pub log: &'a [LogEntry],
    /// What replaying that log produced, for its belief intervals
    pub replay: &'a Replay,
    /// The leader record the elector election actually holds
    pub snapshot: Option<&'a ElectorSnapshot>,
    /// What the configuration's clock assumptions allow
    pub tolerances: Tolerances,
    /// What the configuration expects the electors to have achieved
    pub thresholds: ElectorThresholds,
}

// ============================================================================
// THE MERGED ORDER
// ============================================================================

/// One thing that happened to the elector election
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElectorEvent {
    /// The recipe recorded a leadership transition
    Transition {
        /// What it was
        kind: HistoryKind,
        /// The ballot it produced
        ballot: u64,
        /// Who caused it
        leader_id: String,
    },
    /// A client logged the start of a belief
    BeliefBegin {
        /// Who believed
        client_id: i32,
        /// The term it believed it held
        ballot: u64,
        /// Its own clock when the belief began
        local_nanos: u64,
        /// The horizon it had computed, on its own clock
        horizon_nanos: u64,
    },
    /// A client logged the end of a belief
    BeliefEnd {
        /// Who stopped believing
        client_id: i32,
        /// The term it had believed it held
        ballot: u64,
        /// Its own clock when the belief ended
        local_nanos: u64,
    },
    /// A ranked-register write made under a term
    FencedWrite {
        /// Who wrote
        client_id: i32,
        /// The ballot it wrote under
        ballot: u64,
        /// Whether the register accepted it
        applied: bool,
    },
}

/// One event, with the commit version that orders it against every other
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedEvent {
    /// Commit version of the transaction that produced it
    pub stamp: [u8; 10],
    /// What happened
    pub event: ElectorEvent,
}

/// Put both streams into the order FoundationDB committed them in
///
/// The ten-byte commit version is the whole of the ordering: it is what
/// FoundationDB assigns to a transaction, so two entries from different
/// transactions are ordered by it and nothing else has to be trusted.
///
/// The recipe's transactions and the role's log are always different
/// transactions (the role owns none of the recipe's), so a tie means one
/// transaction wrote both, which cannot happen here. The sort is stable and
/// history comes first anyway, which is the conservative reading of a tie: an
/// acquisition precedes the belief it justifies.
pub fn merge(history: &[StampedTransition], log: &[LogEntry]) -> Vec<MergedEvent> {
    let mut merged: Vec<MergedEvent> = history
        .iter()
        .map(|entry| MergedEvent {
            stamp: entry.stamp,
            event: ElectorEvent::Transition {
                kind: entry.kind,
                ballot: entry.ballot,
                leader_id: entry.leader_id.clone(),
            },
        })
        .collect();

    for entry in log {
        let mut stamp = [0u8; 10];
        stamp.copy_from_slice(&entry.versionstamp[..10]);
        let record = &entry.record;
        let event = match record.op {
            OpKind::BeliefBegin => ElectorEvent::BeliefBegin {
                client_id: entry.client_id,
                ballot: record.ballot,
                local_nanos: record.local_nanos,
                horizon_nanos: record.horizon_nanos,
            },
            OpKind::BeliefEnd => ElectorEvent::BeliefEnd {
                client_id: entry.client_id,
                ballot: record.ballot,
                local_nanos: record.local_nanos,
            },
            OpKind::FencedWrite => ElectorEvent::FencedWrite {
                client_id: entry.client_id,
                ballot: record.ballot,
                applied: record.outcome.is_applied(),
            },
            // The elector role writes nothing else. Anything that turns up here
            // came from another workload sharing the subspace, and dropping it
            // is what keeps this check about the elector.
            _ => continue,
        };
        merged.push(MergedEvent { stamp, event });
    }

    merged.sort_by_key(|entry| entry.stamp);
    merged
}

// ============================================================================
// THE EVIDENCE WINDOW
// ============================================================================

/// The lowest ballot whose acquisition is certainly in the trail, if any
///
/// Retention trims the history from the front
/// ([`with_history_retention`]), so a long run hands back a *suffix* of its own
/// transitions and anything older is simply not evidence. Ballots only ever
/// increase, which is what makes the boundary computable:
///
/// - the suffix opens on an acquisition at `b`: every acquisition from `b`
///   upwards survived, because they all happened at or after it;
/// - the suffix opens on a resign at `b`: the acquisition of `b` itself was
///   trimmed, so only ballots above `b` are covered;
/// - the suffix is empty: nothing is covered.
///
/// This is about what the checks are entitled to *judge*, never about what they
/// are willing to excuse. Inside the window every check is the full-strength
/// one; outside it there is no evidence to be right or wrong about.
///
/// [`with_history_retention`]: foundationdb::recipes::leader_election::LeaderElection::with_history_retention
pub fn first_judgeable_ballot(merged: &[MergedEvent]) -> Option<u64> {
    merged.iter().find_map(|entry| match &entry.event {
        ElectorEvent::Transition {
            kind: HistoryKind::Claim | HistoryKind::Steal,
            ballot,
            ..
        } => Some(*ballot),
        ElectorEvent::Transition {
            kind: HistoryKind::Resign,
            ballot,
            ..
        } => Some(ballot.saturating_add(1)),
        _ => None,
    })
}

/// Applied fenced writes that landed before the trail's first transition
///
/// Reported by the check phase so a trimmed run says how much of itself went
/// unjudged. A number that climbs across runs means the retention bound is too
/// small for the churn, not that the fencing got weaker.
pub fn writes_outside_the_window(merged: &[MergedEvent]) -> usize {
    merged
        .iter()
        .take_while(|entry| !matches!(entry.event, ElectorEvent::Transition { .. }))
        .filter(|entry| matches!(entry.event, ElectorEvent::FencedWrite { applied: true, .. }))
        .count()
}

// ============================================================================
// 1. TERMS FROM HISTORY
// ============================================================================

/// The recipe's own trail describes a succession of terms.
///
/// Ballots move by exactly one per acquisition, never reset and never skip: the
/// ballot is the fencing token, so a reset would silently revalidate every rank
/// a deposed leader still holds. A ballot is acquired once. A resign preserves
/// the ballot it found, which is what lets the successor take `ballot + 1` with
/// no wait at all.
///
/// Retention trims the trail from the front, so the first entry is taken as it
/// comes: whatever it says about a term is the baseline, and everything after
/// it is held to the rules.
pub fn elector_terms_from_history(history: &[StampedTransition]) -> InvariantReport {
    let mut violations = Vec::new();
    let mut current: Option<u64> = None;
    let mut held = false;
    let mut acquired: HashMap<u64, usize> = HashMap::new();

    for (index, entry) in history.iter().enumerate() {
        match entry.kind {
            HistoryKind::Claim | HistoryKind::Steal => {
                if let Some(previous) = current {
                    if entry.ballot != previous + 1 {
                        violations.push(Violation::at(
                            index,
                            format!(
                                "took ballot {} where the trail demands {}: ballots never \
                                 reset and never skip",
                                entry.ballot,
                                previous + 1
                            ),
                        ));
                    }
                }
                match acquired.get(&entry.ballot) {
                    Some(&first) => violations.push(Violation::spanning(
                        vec![first, index],
                        format!(
                            "ballot {} was acquired twice, by {} and by {}: a ballot names \
                             one term",
                            entry.ballot, history[first].leader_id, entry.leader_id
                        ),
                    )),
                    None => {
                        acquired.insert(entry.ballot, index);
                    }
                }
                current = Some(entry.ballot);
                held = true;
            }
            HistoryKind::Resign => {
                match current {
                    // The trail is trimmed from the front, so a resign in first
                    // position is a term this build never saw taken rather than
                    // a term that was never taken.
                    None => {}
                    Some(previous) => {
                        if entry.ballot != previous {
                            violations.push(Violation::at(
                                index,
                                format!(
                                    "a resign preserves ballot {previous}, it recorded {}: \
                                     the successor would reuse a ballot",
                                    entry.ballot
                                ),
                            ));
                        }
                        if !held {
                            violations.push(Violation::at(
                                index,
                                format!("ballot {} was given up twice", entry.ballot),
                            ));
                        }
                    }
                }
                current = Some(entry.ballot);
                held = false;
            }
        }
    }

    InvariantReport::new("ElectorTermsFromHistory", violations)
}

// ============================================================================
// 2. FENCING HOLDS
// ============================================================================

/// A fenced write lands only inside the term that authorized it.
///
/// This is the safety property the elector exists to provide, stated entirely
/// in terms of effects: walk the merged order, keep track of who holds what,
/// and check every write the register accepted against the term that was open
/// when it landed. A leader that stalled past its lease and came back has its
/// write refused by the fence its successor installed, and a write that got
/// through anyway shows up here whatever the code did to produce it.
pub fn elector_fencing_holds(
    merged: &[MergedEvent],
    leader_id: impl Fn(i32) -> String,
) -> InvariantReport {
    let mut violations = Vec::new();
    let mut term: Option<(u64, String)> = None;
    // Until the trail's first transition there is no term evidence at all, so a
    // write there is unjudgeable rather than unauthorized. From the first
    // transition on, the walk always knows who holds what, and every applied
    // write is judged in full: a write at a ballot from the trimmed era that
    // lands inside the window is exactly the violation this exists to catch,
    // and it is caught by the `term` mismatch below like any other.
    let mut window_open = false;

    for (index, entry) in merged.iter().enumerate() {
        match &entry.event {
            ElectorEvent::Transition {
                kind: HistoryKind::Claim | HistoryKind::Steal,
                ballot,
                leader_id,
            } => {
                window_open = true;
                term = Some((*ballot, leader_id.clone()));
            }
            ElectorEvent::Transition {
                kind: HistoryKind::Resign,
                ..
            } => {
                window_open = true;
                term = None;
            }
            ElectorEvent::FencedWrite {
                client_id,
                ballot,
                applied: true,
            } if window_open => {
                let writer = leader_id(*client_id);
                match &term {
                    None => violations.push(Violation::at(
                        index,
                        format!(
                            "{writer} committed a write fenced at ballot {ballot} while \
                             nobody held the term"
                        ),
                    )),
                    Some((held, holder)) if *held != *ballot || *holder != writer => {
                        violations.push(Violation::at(
                            index,
                            format!(
                                "{writer} committed a write fenced at ballot {ballot} while \
                                 {holder} held ballot {held}"
                            ),
                        ));
                    }
                    Some(_) => {}
                }
            }
            _ => {}
        }
    }

    InvariantReport::new("ElectorFencingHolds", violations)
}

// ============================================================================
// 3. NO BELIEF OVERLAP
// ============================================================================

/// No two of the elector's clients believe they lead at the same time.
///
/// The same property, and deliberately the same code, as the driver's
/// [`no_belief_overlap`](super::invariants::no_belief_overlap): the belief
/// intervals are built by the same replay, from records written by the same
/// journal, and a client that was killed is held to the last horizon it logged.
/// What differs is who produced them, which is the whole point: here the
/// horizons are the recipe's own, read off [`LeaseHandle::believed_until`],
/// rather than the driver's reimplementation of them.
///
/// [`LeaseHandle::believed_until`]: foundationdb::recipes::leader_election::LeaseHandle::believed_until
pub fn elector_no_belief_overlap(replay: &Replay, tolerances: &Tolerances) -> InvariantReport {
    let report = super::invariants::no_belief_overlap(replay, tolerances);
    InvariantReport::new("ElectorNoBeliefOverlap", report.violations)
}

// ============================================================================
// 4. BELIEF HONEST
// ============================================================================

/// A logged belief is one the recipe actually granted.
///
/// Two ways a belief record could be a fiction, and both are refused:
///
/// - a begin whose term the history never granted to that client. The recipe
///   writes the history entry in the transaction that wins the term, so an
///   acquisition always precedes, in commit order, any belief it justifies;
/// - an end written at or after the horizon of the belief it closes. Past the
///   horizon there is nothing left to end, and a record written then would
///   widen the interval [`elector_no_belief_overlap`] sees beyond what the
///   client was entitled to believe.
pub fn elector_belief_honest(
    merged: &[MergedEvent],
    leader_id: impl Fn(i32) -> String,
) -> InvariantReport {
    let mut violations = Vec::new();
    let mut granted: Vec<(u64, String)> = Vec::new();
    // The furthest horizon each open belief has claimed, on its own clock.
    let mut horizons: HashMap<(i32, u64), u64> = HashMap::new();
    // Only the grant half needs the window: a belief at a ballot below it may
    // be perfectly honest about an acquisition that retention trimmed away. The
    // horizon half is read entirely from the log, which is never trimmed, so it
    // is judged for every belief whatever the trail kept.
    let judgeable_from = first_judgeable_ballot(merged);

    for (index, entry) in merged.iter().enumerate() {
        match &entry.event {
            ElectorEvent::Transition {
                kind: HistoryKind::Claim | HistoryKind::Steal,
                ballot,
                leader_id,
            } => granted.push((*ballot, leader_id.clone())),
            ElectorEvent::BeliefBegin {
                client_id,
                ballot,
                horizon_nanos,
                ..
            } => {
                let believer = leader_id(*client_id);
                let judgeable = judgeable_from.is_some_and(|first| *ballot >= first);
                if judgeable
                    && !granted
                        .iter()
                        .any(|(granted, holder)| granted == ballot && *holder == believer)
                {
                    violations.push(Violation::at(
                        index,
                        format!(
                            "{believer} began believing it held ballot {ballot}, which no \
                             earlier acquisition ever granted it"
                        ),
                    ));
                }
                let horizon = horizons.entry((*client_id, *ballot)).or_insert(0);
                *horizon = (*horizon).max(*horizon_nanos);
            }
            ElectorEvent::BeliefEnd {
                client_id,
                ballot,
                local_nanos,
            } => match horizons.remove(&(*client_id, *ballot)) {
                None => violations.push(Violation::at(
                    index,
                    format!(
                        "{} ended a belief at ballot {ballot} it never began",
                        leader_id(*client_id)
                    ),
                )),
                Some(horizon) => {
                    if *local_nanos >= horizon {
                        violations.push(Violation::at(
                            index,
                            format!(
                                "{} ended its belief at ballot {ballot} at {local_nanos} ns, \
                                 at or past the {horizon} ns horizon it had computed: the \
                                 horizon had already ended it",
                                leader_id(*client_id)
                            ),
                        ));
                    }
                }
            },
            _ => {}
        }
    }

    InvariantReport::new("ElectorBeliefHonest", violations)
}

// ============================================================================
// 5. SNAPSHOT AGREES
// ============================================================================

/// The record the database holds is the one the last transition wrote.
///
/// The history and the leader record are written by the same transaction, so
/// the newest entry of the trail determines the record exactly: an acquisition
/// leaves its ballot held by its author, a resign leaves the same ballot vacant.
/// They part company only if a write escaped its transaction, and then every
/// other check here is reasoning about a fiction.
pub fn elector_snapshot_agrees(
    history: &[StampedTransition],
    snapshot: Option<&ElectorSnapshot>,
) -> InvariantReport {
    let mut violations = Vec::new();

    match (history.last(), snapshot) {
        (None, None) => {}
        (None, Some(snapshot)) => violations.push(Violation::global(format!(
            "the database holds ballot {} but the history recorded no transition at all",
            snapshot.ballot
        ))),
        (Some(last), None) => violations.push(Violation::at(
            history.len() - 1,
            format!(
                "the history ends with {:?} at ballot {} but the database holds no record",
                last.kind, last.ballot
            ),
        )),
        (Some(last), Some(snapshot)) => {
            let expected = match last.kind {
                HistoryKind::Claim | HistoryKind::Steal => ElectorSnapshot {
                    ballot: last.ballot,
                    leader_id: last.leader_id.clone(),
                    vacant: false,
                },
                HistoryKind::Resign => ElectorSnapshot {
                    ballot: last.ballot,
                    leader_id: String::new(),
                    vacant: true,
                },
            };
            if expected != *snapshot {
                violations.push(Violation::at(
                    history.len() - 1,
                    format!(
                        "the history ends with {:?} at ballot {} by {}, which demands \
                         {expected:?}, but the database holds {snapshot:?}",
                        last.kind, last.ballot, last.leader_id
                    ),
                ));
            }
        }
    }

    InvariantReport::new("ElectorSnapshotAgrees", violations)
}

// ============================================================================
// 6. PROGRESS MADE
// ============================================================================

/// The electors actually led, and wrote something under it.
///
/// Every check above holds vacuously over a run in which no elector ever won a
/// term, and two electors that failed to build would produce exactly that. The
/// floors are only ever applied to a run whose plan drew the feature and whose
/// field could spare the clients; the caller decides that with
/// [`elector_clients`](super::roles::elector_clients) and skips this entirely
/// otherwise.
pub fn elector_progress_made(
    history: &[StampedTransition],
    log: &[LogEntry],
    thresholds: &ElectorThresholds,
) -> InvariantReport {
    let mut violations = Vec::new();

    let acquisitions = history
        .iter()
        .filter(|entry| matches!(entry.kind, HistoryKind::Claim | HistoryKind::Steal))
        .count();
    if acquisitions < thresholds.min_acquisitions {
        violations.push(Violation::global(format!(
            "the electors won {acquisitions} term(s), expected at least {}",
            thresholds.min_acquisitions
        )));
    }

    let writes = log
        .iter()
        .filter(|entry| entry.record.op == OpKind::FencedWrite && entry.record.outcome.is_applied())
        .count();
    if writes < thresholds.min_fenced_writes {
        violations.push(Violation::global(format!(
            "{writes} fenced write(s) were applied under those terms, expected at least {}: \
             a term nobody wrote under proves nothing about fencing",
            thresholds.min_fenced_writes
        )));
    }

    InvariantReport::new("ElectorProgressMade", violations)
}

// ============================================================================
// ALL OF THEM
// ============================================================================

/// The invariants this module checks, in order
///
/// Named here so the check phase can report the ones it *skipped*, which it
/// does whenever a run had no elector: an invariant nobody can see the absence
/// of is one that can quietly stop running.
pub const ELECTOR_INVARIANTS: [&str; 6] = [
    "ElectorTermsFromHistory",
    "ElectorFencingHolds",
    "ElectorNoBeliefOverlap",
    "ElectorBeliefHonest",
    "ElectorSnapshotAgrees",
    "ElectorProgressMade",
];

/// Run every elector invariant, in the order they appear in this module
pub fn check_elector(
    evidence: &ElectorEvidence<'_>,
    leader_id: impl Fn(i32) -> String,
) -> Vec<InvariantReport> {
    let merged = merge(evidence.history, evidence.log);
    vec![
        elector_terms_from_history(evidence.history),
        elector_fencing_holds(&merged, &leader_id),
        elector_no_belief_overlap(evidence.replay, &evidence.tolerances),
        elector_belief_honest(&merged, &leader_id),
        elector_snapshot_agrees(evidence.history, evidence.snapshot),
        elector_progress_made(evidence.history, evidence.log, &evidence.thresholds),
    ]
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::leader_election::log_schema::{LogRecord, Outcome};
    use crate::leader_election::replay::replay;

    /// Seconds, in nanoseconds
    const SEC: u64 = 1_000_000_000;
    /// The lease every fixture advertises
    const LEASE: u64 = 10 * SEC;

    fn leader_id(client_id: i32) -> String {
        format!("process_{client_id}")
    }

    fn stamp(order: u64) -> [u8; 10] {
        let mut stamp = [0u8; 10];
        stamp[..8].copy_from_slice(&order.to_be_bytes());
        stamp
    }

    /// Accumulates both streams against one shared commit order
    #[derive(Debug, Default)]
    struct Fixture {
        history: Vec<StampedTransition>,
        log: Vec<LogEntry>,
        order: u64,
    }

    impl Fixture {
        fn next(&mut self) -> u64 {
            self.order += 1;
            self.order
        }

        fn transition(&mut self, kind: HistoryKind, ballot: u64, client_id: i32) {
            let stamp = stamp(self.next());
            self.history.push(StampedTransition {
                stamp,
                kind,
                ballot,
                leader_id: leader_id(client_id),
            });
        }

        fn push(&mut self, client_id: i32, record: LogRecord) {
            let order = self.next();
            let mut versionstamp = [0u8; 12];
            versionstamp[..10].copy_from_slice(&stamp(order));
            self.log.push(LogEntry {
                versionstamp,
                client_id,
                op_num: order,
                record,
            });
        }

        fn belief_begin(&mut self, client_id: i32, ballot: u64, at: u64, horizon: u64) {
            let mut record = LogRecord::new(OpKind::BeliefBegin);
            record.ballot = ballot;
            record.local_nanos = at;
            record.sim_nanos = at;
            record.horizon_nanos = horizon;
            record.lease_nanos = LEASE;
            self.push(client_id, record);
        }

        fn belief_end(&mut self, client_id: i32, ballot: u64, at: u64) {
            let mut record = LogRecord::new(OpKind::BeliefEnd);
            record.ballot = ballot;
            record.local_nanos = at;
            record.sim_nanos = at;
            self.push(client_id, record);
        }

        fn fenced_write(&mut self, client_id: i32, ballot: u64, outcome: Outcome, at: u64) {
            let mut record = LogRecord::new(OpKind::FencedWrite);
            record.ballot = ballot;
            record.outcome = outcome;
            record.local_nanos = at;
            record.sim_nanos = at;
            self.push(client_id, record);
        }

        fn merged(&self) -> Vec<MergedEvent> {
            merge(&self.history, &self.log)
        }

        fn reports(&self, snapshot: &ElectorSnapshot) -> Vec<InvariantReport> {
            let replayed = replay(&self.log, leader_id);
            check_elector(
                &ElectorEvidence {
                    history: &self.history,
                    log: &self.log,
                    replay: &replayed,
                    snapshot: Some(snapshot),
                    tolerances: Tolerances::STRICT,
                    thresholds: ElectorThresholds::ACTIVE,
                },
                leader_id,
            )
        }
    }

    /// The canonical well-behaved run of two electors.
    ///
    /// Client 6 takes ballot 1, fences a write, ends its belief and resigns.
    /// Client 7 reclaims the vacancy at ballot 2 and fences a write of its own.
    /// Client 6 then tries one more write under its dead term, and the fence
    /// client 7 installed refuses it.
    fn clean() -> Fixture {
        let mut run = Fixture::default();

        run.transition(HistoryKind::Claim, 1, 6);
        run.belief_begin(6, 1, SEC, 9 * SEC);
        run.fenced_write(6, 1, Outcome::Applied, 2 * SEC);
        run.belief_end(6, 1, 5 * SEC);
        run.transition(HistoryKind::Resign, 1, 6);

        run.transition(HistoryKind::Claim, 2, 7);
        run.belief_begin(7, 2, 6 * SEC, 14 * SEC);
        run.fenced_write(7, 2, Outcome::Applied, 7 * SEC);
        run.fenced_write(6, 1, Outcome::Rejected, 8 * SEC);

        run
    }

    fn clean_snapshot() -> ElectorSnapshot {
        ElectorSnapshot {
            ballot: 2,
            leader_id: leader_id(7),
            vacant: false,
        }
    }

    /// Assert the invariant failed, *and* that it failed for the reason the
    /// test set out to provoke: without the second half a mutation that trips
    /// some unrelated check would look like a passing falsification test.
    fn assert_failed(report: &InvariantReport, because: &str) {
        assert!(
            !report.passed(),
            "{} passed on a run built to break it",
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

    #[test]
    fn the_clean_run_satisfies_every_invariant() {
        for report in clean().reports(&clean_snapshot()) {
            assert!(
                report.passed(),
                "{} failed on the clean run: {:?}",
                report.name,
                report.violations
            );
        }
    }

    #[test]
    fn the_merge_puts_both_streams_in_commit_order() {
        let run = clean();
        let merged = run.merged();
        assert_eq!(merged.len(), run.history.len() + run.log.len());
        for pair in merged.windows(2) {
            assert!(pair[0].stamp < pair[1].stamp, "the merge is not ordered");
        }
        // And it really is interleaved: a history entry sits between two log
        // entries, which is the only reason the merge is worth doing.
        assert!(matches!(
            merged[4].event,
            ElectorEvent::Transition {
                kind: HistoryKind::Resign,
                ..
            }
        ));
    }

    // ---- 1. terms from history -------------------------------------------

    #[test]
    fn a_skipped_ballot_is_caught() {
        let mut run = clean();
        run.history[2].ballot = 4;
        assert_failed(
            &elector_terms_from_history(&run.history),
            "ballots never reset and never skip",
        );
    }

    #[test]
    fn a_ballot_acquired_twice_is_caught() {
        let mut run = Fixture::default();
        run.transition(HistoryKind::Claim, 1, 6);
        run.transition(HistoryKind::Resign, 1, 6);
        // A second acquisition at a ballot somebody already held: two processes
        // would hold ranks neither of which dominates the other.
        run.history.push(StampedTransition {
            stamp: stamp(99),
            kind: HistoryKind::Claim,
            ballot: 1,
            leader_id: leader_id(7),
        });
        assert_failed(
            &elector_terms_from_history(&run.history),
            "was acquired twice",
        );
    }

    #[test]
    fn a_resign_that_moves_the_ballot_is_caught() {
        let mut run = clean();
        run.history[1].ballot = 7;
        assert_failed(
            &elector_terms_from_history(&run.history),
            "a resign preserves ballot 1",
        );
    }

    #[test]
    fn a_trimmed_trail_is_read_from_wherever_it_starts() {
        // Retention drops the oldest entries, so a trail beginning at ballot 57
        // with a resign is a term this build never saw taken, not a defect.
        let mut run = Fixture::default();
        run.transition(HistoryKind::Resign, 57, 6);
        run.transition(HistoryKind::Claim, 58, 7);
        assert!(elector_terms_from_history(&run.history).passed());
    }

    // ---- the evidence window ---------------------------------------------

    /// The live failure this fix came from: seed 7285800 read a one-entry
    /// history on one client and seven on another, because the recipe's
    /// `history` returned a single batch of a reverse scan. The read is fixed,
    /// but retention trims the same way on purpose, so the shape has to be
    /// handled rather than assumed away.
    fn truncated() -> Fixture {
        let mut run = clean();
        // Retention kept only the newest entry: client 6's whole term at
        // ballot 1 is gone from the trail, while its log records survive.
        run.history = run.history.split_off(2);
        run
    }

    #[test]
    fn a_trimmed_trail_does_not_invent_fencing_violations() {
        // Every record of client 6's term at ballot 1 is still in the log, and
        // none of it is evidence of anything without the transitions that
        // granted it. This is the exact false positive the live run hit.
        let run = truncated();
        assert_eq!(run.history.len(), 1, "the fixture must reproduce the shape");

        for report in run.reports(&clean_snapshot()) {
            assert!(
                report.passed(),
                "{} failed on a trimmed but honest run: {:?}",
                report.name,
                report.violations
            );
        }
        assert_eq!(writes_outside_the_window(&run.merged()), 1);
    }

    #[test]
    fn the_window_opens_where_the_trail_does() {
        let full = clean();
        // The trail opens on the acquisition of ballot 1, so ballot 1 is
        // covered.
        assert_eq!(first_judgeable_ballot(&full.merged()), Some(1));

        // It opens on the acquisition of ballot 2, so ballot 1 is not.
        assert_eq!(first_judgeable_ballot(&truncated().merged()), Some(2));

        // A trail opening on a resign cannot vouch for that ballot's own
        // acquisition, which was trimmed with everything before it.
        let mut resign_first = clean();
        resign_first.history = resign_first.history.split_off(1);
        assert_eq!(first_judgeable_ballot(&resign_first.merged()), Some(2));

        // No trail, nothing to judge.
        let mut empty = clean();
        empty.history.clear();
        assert_eq!(first_judgeable_ballot(&empty.merged()), None);
        assert!(elector_fencing_holds(&empty.merged(), leader_id).passed());
    }

    #[test]
    fn a_stale_write_inside_the_window_is_still_caught_on_a_trimmed_trail() {
        // The anti-regression for the fix above. The window skips what precedes
        // the trail, not every write at an old ballot: client 6's write under
        // its dead ballot 1 lands *after* client 7 took ballot 2, and that is a
        // real fencing violation whether or not ballot 1's own term survived
        // retention.
        let mut run = truncated();
        let stale = run
            .log
            .iter_mut()
            .find(|entry| entry.record.outcome == Outcome::Rejected)
            .expect("the fixture contains a refused write");
        stale.record.outcome = Outcome::Applied;

        assert_failed(
            &elector_fencing_holds(&run.merged(), leader_id),
            "while process_7 held ballot 2",
        );
    }

    #[test]
    fn a_dishonest_belief_inside_the_window_is_still_caught_on_a_trimmed_trail() {
        // Same guarantee for the grant check: ballot 2 is inside the window, so
        // a belief in it by somebody the trail never granted it to still fires.
        let mut run = truncated();
        run.history[0].leader_id = leader_id(6);
        assert_failed(
            &elector_belief_honest(&run.merged(), leader_id),
            "which no earlier acquisition ever granted it",
        );
    }

    #[test]
    fn a_belief_end_past_its_horizon_is_caught_even_outside_the_window() {
        // The horizon half reads only the log, which retention never touches,
        // so trimming the trail must not buy a client the right to claim it
        // believed longer than it did.
        let mut run = truncated();
        let end = run
            .log
            .iter_mut()
            .find(|entry| entry.record.op == OpKind::BeliefEnd)
            .expect("the fixture contains a belief end");
        end.record.local_nanos = 9 * SEC;
        assert_failed(
            &elector_belief_honest(&run.merged(), leader_id),
            "the horizon had already ended it",
        );
    }

    // ---- 2. fencing holds -------------------------------------------------

    #[test]
    fn a_stale_write_that_landed_is_caught() {
        // The Kleppmann pause, as an effect: client 6's write under its dead
        // ballot 1 committed while client 7 held ballot 2.
        let mut run = clean();
        let stale = run
            .log
            .iter_mut()
            .find(|entry| entry.record.outcome == Outcome::Rejected)
            .expect("the fixture contains a refused write");
        stale.record.outcome = Outcome::Applied;

        assert_failed(
            &elector_fencing_holds(&run.merged(), leader_id),
            "while process_7 held ballot 2",
        );
    }

    #[test]
    fn a_write_under_a_resigned_term_is_caught() {
        let mut run = Fixture::default();
        run.transition(HistoryKind::Claim, 1, 6);
        run.transition(HistoryKind::Resign, 1, 6);
        run.fenced_write(6, 1, Outcome::Applied, 3 * SEC);
        assert_failed(
            &elector_fencing_holds(&run.merged(), leader_id),
            "while nobody held the term",
        );
    }

    // ---- 3. no belief overlap --------------------------------------------

    #[test]
    fn two_electors_believing_at_once_are_caught() {
        let mut run = clean();
        // Client 7 starts believing at three seconds, two seconds before client
        // 6 said it had stopped.
        let begin = run
            .log
            .iter_mut()
            .find(|entry| entry.client_id == 7 && entry.record.op == OpKind::BeliefBegin)
            .expect("the fixture contains client 7's belief");
        begin.record.sim_nanos = 3 * SEC;
        begin.record.local_nanos = 3 * SEC;

        let replayed = replay(&run.log, leader_id);
        assert_failed(
            &elector_no_belief_overlap(&replayed, &Tolerances::STRICT),
            "of overlap",
        );
    }

    #[test]
    fn a_killed_elector_is_held_to_the_horizon_it_logged() {
        // No belief end at all: the client stopped responding. It is held to
        // the horizon it computed, and a successor that starts before that is
        // an overlap even though nobody ever wrote an end.
        let mut run = Fixture::default();
        run.transition(HistoryKind::Claim, 1, 6);
        run.belief_begin(6, 1, SEC, 9 * SEC);
        run.transition(HistoryKind::Steal, 2, 7);
        run.belief_begin(7, 2, 5 * SEC, 13 * SEC);

        let replayed = replay(&run.log, leader_id);
        assert_failed(
            &elector_no_belief_overlap(&replayed, &Tolerances::STRICT),
            "of overlap",
        );
    }

    // ---- 4. belief honest -------------------------------------------------

    #[test]
    fn a_belief_at_a_term_nobody_granted_is_caught() {
        let mut run = clean();
        // Client 7 believes it holds ballot 2, which the history granted to
        // client 6.
        run.history[2].leader_id = leader_id(6);
        assert_failed(
            &elector_belief_honest(&run.merged(), leader_id),
            "which no earlier acquisition ever granted it",
        );
    }

    #[test]
    fn a_belief_end_written_past_its_horizon_is_caught() {
        let mut run = clean();
        let end = run
            .log
            .iter_mut()
            .find(|entry| entry.record.op == OpKind::BeliefEnd)
            .expect("the fixture contains a belief end");
        end.record.local_nanos = 9 * SEC;
        assert_failed(
            &elector_belief_honest(&run.merged(), leader_id),
            "the horizon had already ended it",
        );
    }

    // ---- 5. snapshot agrees -----------------------------------------------

    #[test]
    fn a_record_the_history_does_not_explain_is_caught() {
        let run = clean();
        let snapshot = ElectorSnapshot {
            ballot: 3,
            leader_id: leader_id(7),
            vacant: false,
        };
        assert_failed(
            &elector_snapshot_agrees(&run.history, Some(&snapshot)),
            "but the database holds",
        );
    }

    #[test]
    fn a_resign_must_leave_the_record_vacant() {
        let mut run = clean();
        run.transition(HistoryKind::Resign, 2, 7);
        assert_failed(
            &elector_snapshot_agrees(&run.history, Some(&clean_snapshot())),
            "but the database holds",
        );

        let vacant = ElectorSnapshot {
            ballot: 2,
            leader_id: String::new(),
            vacant: true,
        };
        assert!(elector_snapshot_agrees(&run.history, Some(&vacant)).passed());
    }

    // ---- 6. progress made -------------------------------------------------

    #[test]
    fn a_run_where_no_elector_led_is_caught() {
        let empty = Fixture::default();
        let report = elector_progress_made(&empty.history, &empty.log, &ElectorThresholds::ACTIVE);
        assert_failed(&report, "won 0 term(s)");
        assert_failed(&report, "fenced write(s) were applied");
    }

    #[test]
    fn a_term_nobody_wrote_under_proves_nothing() {
        let mut run = Fixture::default();
        run.transition(HistoryKind::Claim, 1, 6);
        run.belief_begin(6, 1, SEC, 9 * SEC);
        assert_failed(
            &elector_progress_made(&run.history, &run.log, &ElectorThresholds::ACTIVE),
            "proves nothing about fencing",
        );
    }

    #[test]
    fn the_published_names_are_the_ones_that_run() {
        // The check phase traces the skipped invariants from the constant
        // rather than from a run, so a name that drifted would make a skipped
        // invariant unreportable, or report one that no longer exists.
        let names: Vec<&str> = clean()
            .reports(&clean_snapshot())
            .iter()
            .map(|report| report.name)
            .collect();
        assert_eq!(names, ELECTOR_INVARIANTS.to_vec());

        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "{names:?}");
    }
}
