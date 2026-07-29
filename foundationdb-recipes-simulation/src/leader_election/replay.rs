//! Commit-ordered replay of the operation log.
//!
//! Replay is a straight fold: it applies the writes the log says happened, in
//! the order FoundationDB committed them, and reconstructs what the leader
//! record must look like as a result. It deliberately judges nothing. If a log
//! claims a claim jumped two ballots, replay believes it and moves the state
//! two ballots; catching that is [`invariants`](super::invariants)' job.
//!
//! Keeping the two apart is what makes the invariants falsifiable. A replay
//! that quietly repaired or rejected impossible sequences would make every
//! downstream check pass by construction, which is exactly how the previous
//! suite ended up with seven invariants that could never fail.
//!
//! Only entries with `leader_record_written` move the state. A recovered
//! unknown commit reports success and writes nothing, so it must not be
//! counted as a second transition.

use std::collections::HashMap;

use super::log_schema::{LogEntry, ObservedIdentity, OpKind};

/// The leader record as the log says it should be
///
/// Mirrors the recipe's stored record, minus the parts replay cannot know.
/// The vacancy sentinel is the same: an all-zero token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedRecord {
    /// Term number
    pub ballot: u64,
    /// Renewal counter within the term
    pub generation: u64,
    /// Holder, empty when vacant
    pub leader_id: String,
    /// Per-term token, all-zero when vacant
    pub token: [u8; 16],
    /// Advertised lease, zero when vacant
    pub lease_nanos: u64,
}

impl ExpectedRecord {
    /// Whether the term was resigned rather than held
    pub fn is_vacant(&self) -> bool {
        self.token == [0u8; 16]
    }

    /// The `(ballot, generation)` pair observers track
    pub fn identity(&self) -> ObservedIdentity {
        ObservedIdentity {
            ballot: self.ballot,
            generation: self.generation,
            vacant: self.is_vacant(),
        }
    }
}

/// The kind of leader-record mutation a transition performed
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransitionKind {
    /// Took an absent or vacant record
    Claim,
    /// Took a record from a holder
    Steal,
    /// Bumped the generation at a fixed ballot
    Renew,
    /// Wrote the vacant record
    Resign,
}

impl TransitionKind {
    fn from_op(op: OpKind) -> Option<Self> {
        match op {
            OpKind::Claim => Some(Self::Claim),
            OpKind::Steal => Some(Self::Steal),
            OpKind::Renew => Some(Self::Renew),
            OpKind::Resign => Some(Self::Resign),
            _ => None,
        }
    }

    /// Whether this transition starts a new term
    pub fn is_acquisition(self) -> bool {
        matches!(self, Self::Claim | Self::Steal)
    }
}

/// One applied mutation of the leader record
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    /// Index of the entry in the replayed slice
    pub index: usize,
    /// Which client committed it
    pub client_id: i32,
    /// What it did
    pub kind: TransitionKind,
    /// The ballot it wrote
    pub ballot: u64,
    /// The generation it wrote
    pub generation: u64,
    /// The leader id derived from the writing client
    pub leader_id: String,
    /// The token it wrote, all-zero for a resign
    pub token: [u8; 16],
    /// What the transaction read before deciding
    pub observed: Option<ObservedIdentity>,
}

/// One leadership interval, from acquisition to the transition that ended it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term {
    /// The term number
    pub ballot: u64,
    /// The client that held it
    pub client_id: i32,
    /// The leader id it was held under
    pub leader_id: String,
    /// The per-term token
    pub token: [u8; 16],
    /// The lease it advertised
    pub lease_nanos: u64,
    /// Entry index of the claim or steal that started it
    pub start: usize,
    /// Entry index of the resign or steal that ended it, if it ended
    pub end: Option<usize>,
    /// Entry indices of the renewals inside it
    pub renewals: Vec<usize>,
}

impl Term {
    /// Whether this term was still held at `index`
    pub fn covers(&self, index: usize) -> bool {
        index >= self.start && self.end.is_none_or(|end| index < end)
    }
}

/// One interval during which a client believed it led
///
/// Written by the driver loop rather than derived: only the driver knows when
/// it stopped believing, and a leader that was killed never gets to say so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Belief {
    /// The client that believed
    pub client_id: i32,
    /// The term it believed it held
    pub ballot: u64,
    /// Entry index of the belief-begin record
    pub begin_index: usize,
    /// Entry index of the belief-end record, if the client ever said so
    pub end_index: Option<usize>,
    /// True simulated time the belief began
    pub begin_sim_nanos: u64,
    /// True simulated time the belief ended, if it did
    pub end_sim_nanos: Option<u64>,
    /// The client's own clock when the belief began
    pub begin_local_nanos: u64,
    /// The horizon the client computed, on its own clock; the latest one wins
    /// when renewals extend the belief
    pub horizon_nanos: u64,
}

impl Belief {
    /// When this belief must be considered over, in simulated time.
    ///
    /// A belief that was explicitly ended is over then. One that was not (the
    /// client was killed) is bounded by the horizon the client had computed:
    /// it stops believing at that point whether or not it is alive to say so.
    /// The horizon was computed on the client's own clock, so translating it
    /// into simulated time is only as good as the clock-rate bound the
    /// configuration assumes, which is why the caller adds a tolerance.
    pub fn effective_end_sim_nanos(&self) -> u64 {
        match self.end_sim_nanos {
            Some(end) => end,
            None => self
                .begin_sim_nanos
                .saturating_add(self.horizon_nanos.saturating_sub(self.begin_local_nanos)),
        }
    }
}

/// A log entry that does not make sense on its own terms
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anomaly {
    /// Index of the offending entry
    pub index: usize,
    /// What is wrong with it
    pub detail: String,
}

/// What replaying a log produced
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Replay {
    /// The record the log says the database should end up holding
    pub final_state: Option<ExpectedRecord>,
    /// The state just before each entry, indexed like the entry slice
    pub states_before: Vec<Option<ExpectedRecord>>,
    /// The applied mutations, in commit order
    pub transitions: Vec<Transition>,
    /// The leadership intervals
    pub terms: Vec<Term>,
    /// The belief intervals the drivers reported
    pub beliefs: Vec<Belief>,
    /// Entries that are self-contradictory (a fenced write that claims to have
    /// mutated the leader record, a belief that ends without beginning)
    pub anomalies: Vec<Anomaly>,
}

impl Replay {
    /// The transition that produced `identity`, searching backwards from
    /// `before`
    pub fn transition_producing(
        &self,
        identity: ObservedIdentity,
        before: usize,
    ) -> Option<&Transition> {
        self.transitions
            .iter()
            .rev()
            .filter(|t| t.index < before)
            .find(|t| {
                t.ballot == identity.ballot
                    && t.generation == identity.generation
                    && (t.kind == TransitionKind::Resign) == identity.vacant
            })
    }

    /// The term that was held at `index`, if any
    pub fn term_at(&self, index: usize) -> Option<&Term> {
        self.terms.iter().find(|term| term.covers(index))
    }
}

/// Replay a log in commit order.
///
/// `leader_id` maps a client id to the identifier that client claims under;
/// the log stores the client id and the workload derives the identifier from
/// it, so replay needs the same mapping to reconstruct the record.
pub fn replay(entries: &[LogEntry], leader_id: impl Fn(i32) -> String) -> Replay {
    let mut out = Replay {
        states_before: Vec::with_capacity(entries.len()),
        ..Replay::default()
    };
    let mut state: Option<ExpectedRecord> = None;
    let mut open_beliefs: HashMap<(i32, u64), usize> = HashMap::new();
    let mut open_term: Option<usize> = None;

    for (index, entry) in entries.iter().enumerate() {
        out.states_before.push(state.clone());
        let record = &entry.record;

        match record.op {
            OpKind::BeliefBegin => {
                let key = (entry.client_id, record.ballot);
                match open_beliefs.get(&key) {
                    // A renewal extended the belief: keep the interval open and
                    // take the later horizon.
                    Some(&existing) => {
                        let belief = &mut out.beliefs[existing];
                        belief.horizon_nanos = belief.horizon_nanos.max(record.horizon_nanos);
                    }
                    None => {
                        open_beliefs.insert(key, out.beliefs.len());
                        out.beliefs.push(Belief {
                            client_id: entry.client_id,
                            ballot: record.ballot,
                            begin_index: index,
                            end_index: None,
                            begin_sim_nanos: record.sim_nanos,
                            end_sim_nanos: None,
                            begin_local_nanos: record.local_nanos,
                            horizon_nanos: record.horizon_nanos,
                        });
                    }
                }
            }
            OpKind::BeliefEnd => match open_beliefs.remove(&(entry.client_id, record.ballot)) {
                Some(existing) => {
                    let belief = &mut out.beliefs[existing];
                    belief.end_index = Some(index);
                    belief.end_sim_nanos = Some(record.sim_nanos);
                }
                None => out.anomalies.push(Anomaly {
                    index,
                    detail: format!(
                        "belief end at ballot {} without a matching begin",
                        record.ballot
                    ),
                }),
            },
            _ => {}
        }

        if !record.leader_record_written {
            continue;
        }

        let kind = match TransitionKind::from_op(record.op) {
            Some(kind) => kind,
            None => {
                out.anomalies.push(Anomaly {
                    index,
                    detail: format!("op {} cannot write the leader record", record.op),
                });
                continue;
            }
        };
        if !record.outcome.is_applied() {
            out.anomalies.push(Anomaly {
                index,
                detail: "a rejected op reports having written the leader record".to_string(),
            });
        }
        if record.recovery_noop {
            out.anomalies.push(Anomaly {
                index,
                detail: "a recovery no-op reports having written the leader record".to_string(),
            });
        }

        let id = leader_id(entry.client_id);
        let written = match kind {
            TransitionKind::Resign => ExpectedRecord {
                ballot: record.ballot,
                generation: record.generation,
                leader_id: String::new(),
                token: [0u8; 16],
                lease_nanos: 0,
            },
            _ => ExpectedRecord {
                ballot: record.ballot,
                generation: record.generation,
                leader_id: id.clone(),
                token: record.token,
                lease_nanos: record.lease_nanos,
            },
        };

        match kind {
            TransitionKind::Claim | TransitionKind::Steal => {
                if let Some(previous) = open_term.take() {
                    out.terms[previous].end = Some(index);
                }
                open_term = Some(out.terms.len());
                out.terms.push(Term {
                    ballot: record.ballot,
                    client_id: entry.client_id,
                    leader_id: id.clone(),
                    token: record.token,
                    lease_nanos: record.lease_nanos,
                    start: index,
                    end: None,
                    renewals: Vec::new(),
                });
            }
            TransitionKind::Renew => {
                if let Some(current) = open_term {
                    out.terms[current].renewals.push(index);
                }
            }
            TransitionKind::Resign => {
                if let Some(previous) = open_term.take() {
                    out.terms[previous].end = Some(index);
                }
            }
        }

        out.transitions.push(Transition {
            index,
            client_id: entry.client_id,
            kind,
            ballot: record.ballot,
            generation: record.generation,
            leader_id: id,
            token: record.token,
            observed: record.observed,
        });
        state = Some(written);
    }

    out.final_state = state;
    out
}

#[cfg(test)]
mod tests {
    use super::super::log_schema::fixtures::*;
    use super::super::log_schema::{LogRecord, OpKind, Outcome};
    use super::*;

    fn replay_clean() -> Replay {
        replay(&clean_log(), leader_id)
    }

    #[test]
    fn a_claim_renew_resign_sequence_ends_vacant_at_the_same_ballot() {
        let mut log = LogBuilder::new();
        log.push(0, write(OpKind::Claim, 1, 0, 1, SEC));
        log.push(0, write(OpKind::Renew, 1, 1, 1, 2 * SEC));
        log.push(0, write(OpKind::Renew, 1, 2, 1, 3 * SEC));
        log.push(0, resign(1, 2, 1, 4 * SEC));

        let out = replay(log.entries(), leader_id);
        let state = out.final_state.expect("the log wrote a record");
        assert!(state.is_vacant());
        assert_eq!(state.ballot, 1, "a resign preserves the ballot");
        assert_eq!(state.generation, 2);
        assert_eq!(out.transitions.len(), 4);
        assert_eq!(out.terms.len(), 1);
        assert_eq!(out.terms[0].renewals, vec![1, 2]);
        assert_eq!(out.terms[0].end, Some(3));
    }

    #[test]
    fn a_vacant_record_is_reclaimed_at_the_next_ballot() {
        let mut log = LogBuilder::new();
        log.push(0, write(OpKind::Claim, 1, 0, 1, SEC));
        log.push(0, resign(1, 0, 1, 2 * SEC));
        let mut reclaim = write(OpKind::Claim, 2, 0, 2, 3 * SEC);
        observed(&mut reclaim, 1, 0, true);
        log.push(1, reclaim);

        let out = replay(log.entries(), leader_id);
        let state = out.final_state.expect("the log wrote a record");
        assert_eq!(state.ballot, 2);
        assert_eq!(state.leader_id, leader_id(1));
        assert!(!state.is_vacant());
        assert_eq!(out.terms.len(), 2);
        assert_eq!(out.terms[1].client_id, 1);
    }

    #[test]
    fn a_steal_closes_the_victims_term() {
        let out = replay_clean();
        let stolen = out
            .terms
            .iter()
            .find(|term| term.ballot == 2)
            .expect("client 1 held ballot 2");
        let stealer = out
            .terms
            .iter()
            .find(|term| term.ballot == 3)
            .expect("client 2 stole ballot 3");
        assert_eq!(stolen.end, Some(stealer.start));
        assert_eq!(stealer.client_id, 2);
        assert_eq!(out.final_state.as_ref().unwrap(), &clean_snapshot());
    }

    #[test]
    fn a_recovered_unknown_commit_is_not_a_second_transition() {
        // The whole point of `leader_record_written`: the recovery reports the
        // same ballot and the same token as the claim it recovered, and replay
        // must not read that as two applied claims.
        let out = replay_clean();
        let claims: Vec<_> = out
            .transitions
            .iter()
            .filter(|t| t.kind == TransitionKind::Claim && t.ballot == 2)
            .collect();
        assert_eq!(claims.len(), 1);

        // Flipping just that flag is what would double-count it.
        let mut entries = clean_log();
        let recovery = entries
            .iter_mut()
            .find(|entry| entry.record.recovery_noop)
            .expect("the fixture contains a recovery");
        recovery.record.leader_record_written = true;
        let mutated = replay(&entries, leader_id);
        assert_eq!(
            mutated
                .transitions
                .iter()
                .filter(|t| t.kind == TransitionKind::Claim && t.ballot == 2)
                .count(),
            2
        );
        assert!(!mutated.anomalies.is_empty());
    }

    #[test]
    fn an_injected_unknown_marker_is_not_a_transition() {
        // The marker says a reply was thrown away, not that anything was
        // written: the claim it follows is the only transition of the pair, and
        // counting the marker would put the log one acquisition ahead of the
        // database.
        let entries = injected_recovery_log();
        let out = replay(&entries, leader_id);
        assert!(out.anomalies.is_empty(), "{:?}", out.anomalies);
        assert_eq!(
            out.final_state.as_ref(),
            Some(&injected_recovery_snapshot())
        );

        let markers: Vec<usize> = entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.record.op == OpKind::InjectedUnknown)
            .map(|(index, _)| index)
            .collect();
        assert_eq!(markers.len(), 2);
        for index in markers {
            assert!(out.transitions.iter().all(|t| t.index != index));
        }
        // Three claims and a steal: neither the adoption nor the superseded
        // re-probe is among them.
        assert_eq!(
            out.transitions
                .iter()
                .filter(|t| t.kind.is_acquisition())
                .count(),
            4
        );
    }

    #[test]
    fn rejected_operations_leave_the_state_alone() {
        let out = replay_clean();
        // The paused client's stale renewal is in the log and changed nothing.
        let stale = clean_log()
            .iter()
            .position(|entry| {
                entry.record.outcome == Outcome::Rejected && entry.record.op == OpKind::Renew
            })
            .expect("the fixture contains a rejected renewal");
        assert!(out.transitions.iter().all(|t| t.index != stale));
        assert_eq!(out.final_state.as_ref().unwrap().ballot, 3);
    }

    #[test]
    fn beliefs_are_paired_and_extended_by_renewals() {
        let out = replay_clean();
        assert_eq!(out.beliefs.len(), 3);

        let first = &out.beliefs[0];
        assert_eq!(first.client_id, 0);
        assert_eq!(first.end_sim_nanos, Some(6 * SEC));
        // The renewal's belief-begin pushed the horizon out.
        assert_eq!(first.horizon_nanos, 13 * SEC);

        // Client 1 was killed and never reported an end, so its belief is
        // bounded by the horizon it had computed.
        let crashed = &out.beliefs[1];
        assert_eq!(crashed.end_sim_nanos, None);
        assert_eq!(crashed.effective_end_sim_nanos(), 15 * SEC + 4 * SEC / 10);
    }

    #[test]
    fn an_unmatched_belief_end_is_an_anomaly() {
        let mut log = LogBuilder::new();
        log.push(0, belief_end(0, 1, SEC));
        let out = replay(log.entries(), leader_id);
        assert_eq!(out.anomalies.len(), 1);
        assert!(out.beliefs.is_empty());
    }

    #[test]
    fn an_op_that_cannot_write_the_record_but_says_it_did_is_an_anomaly() {
        let mut log = LogBuilder::new();
        let mut record = LogRecord::new(OpKind::FencedWrite);
        record.leader_record_written = true;
        log.push(0, record);
        let out = replay(log.entries(), leader_id);
        assert_eq!(out.anomalies.len(), 1);
        assert!(out.transitions.is_empty());
    }

    #[test]
    fn the_state_before_an_entry_is_what_the_entry_read() {
        let out = replay_clean();
        let entries = clean_log();
        for (index, entry) in entries.iter().enumerate() {
            let observed = match entry.record.observed {
                Some(observed) => observed,
                None => continue,
            };
            // Every fixture op read the state replay says was there.
            match &out.states_before[index] {
                Some(state) => assert_eq!(
                    state.identity(),
                    observed,
                    "entry {index} ({}) observed a state replay does not have",
                    entry.record.op
                ),
                None => panic!("entry {index} observed a record that replay says was absent"),
            }
        }
    }

    #[test]
    fn transition_producing_finds_the_write_behind_an_observation() {
        let out = replay_clean();
        let steal = out
            .transitions
            .iter()
            .find(|t| t.kind == TransitionKind::Steal)
            .expect("the fixture contains a steal");
        let source = out
            .transition_producing(steal.observed.unwrap(), steal.index)
            .expect("the observed identity was written by somebody");
        assert_eq!(source.kind, TransitionKind::Renew);
        assert_eq!(source.ballot, 2);
        assert_eq!(source.generation, 3);
    }
}
