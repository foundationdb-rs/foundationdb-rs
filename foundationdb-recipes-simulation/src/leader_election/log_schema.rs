//! The versionstamped operation log written by the workload.
//!
//! Every primitive the workload drives is wrapped in a transaction that also
//! appends one record here, so the record commits if and only if the operation
//! did. The key carries an incomplete versionstamp, which FoundationDB fills in
//! at commit time: reading the subspace in key order therefore yields the
//! operations in *commit* order, which is the only ordering the check phase can
//! trust.
//!
//! ```text
//! ("le_log", <versionstamp>, client_id, op_num) -> record
//! ```
//!
//! The record is deliberately fatter than the operation it describes. Three
//! groups of fields exist only so the check phase can tell apart things that
//! look identical from the outside:
//!
//! - `leader_record_written` separates transactions that actually mutated the
//!   leader record from those that merely reported an outcome. Only the former
//!   drive replay, so a claim and the recovery that recognized it cannot be
//!   counted as two applied transitions.
//! - `recovery_noop`, `superseded` and `maybe_committed` mark the
//!   unknown-commit path, which is exactly where double-counting would hide.
//! - `local_nanos` (the caller's skewed clock) and `sim_nanos` (true simulated
//!   time, sampled inside the transaction) are kept apart. The recipe only ever
//!   sees the former; the check phase uses the latter as its oracle.
//!
//! Belief records are written by the driver loop, not by the recipe: they say
//! when a client *started and stopped believing* it led, which is the thing
//! `NoBeliefOverlap` needs and which no in-transaction timestamp can
//! reconstruct.

use std::fmt;

use foundationdb::tuple::{Subspace, Versionstamp, pack, unpack};

/// Version of the log record layout, bumped on any change to the value tuple
pub const LOG_SCHEMA_VERSION: u64 = 2;

/// Prefix of the subspace the log lives in
pub const LOG_PREFIX: &str = "le_log";

/// Prefix of the subspace the elector role's log lives in
///
/// The real [`LeaderElector`] runs against an election of its own
/// ([`elector_role`](super::elector_role)), so what its clients record has to
/// land somewhere the driver's replay never sees: the two runs share a
/// database, not a history.
///
/// [`LeaderElector`]: foundationdb::recipes::leader_election::LeaderElector
pub const ELECTOR_LOG_PREFIX: &str = "le_elector_log";

/// The subspace the operation log is written to
pub fn log_subspace() -> Subspace {
    Subspace::all().subspace(&(LOG_PREFIX,))
}

/// The subspace the elector role's log is written to
pub fn elector_log_subspace() -> Subspace {
    Subspace::all().subspace(&(ELECTOR_LOG_PREFIX,))
}

/// A malformed log key or value
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaError(pub String);

impl fmt::Display for SchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SchemaError {}

type Result<T> = std::result::Result<T, SchemaError>;

// ============================================================================
// OP KIND
// ============================================================================

/// What the logged transaction attempted
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpKind {
    /// Take an absent or vacant term
    Claim,
    /// Take a term from a holder whose record was observed to stand still
    Steal,
    /// Bump the generation at a fixed ballot
    Renew,
    /// Write the vacant record
    Resign,
    /// A ranked-register write made under a leadership ballot
    FencedWrite,
    /// A read-only sighting of the leader record
    Observe,
    /// The driver started believing it leads
    BeliefBegin,
    /// The driver stopped believing it leads
    BeliefEnd,
    /// The driver threw away a claim reply it had actually received
    ///
    /// The BUGGIFY-style injection: the marker is written after the claim it
    /// describes committed, so it exists if and only if that claim did. It
    /// mutates nothing, and its only reader is the check phase, which uses it
    /// to tell a run that exercised the recovery path from one that merely
    /// could have.
    InjectedUnknown,
}

impl OpKind {
    /// The wire name
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claim => "claim",
            Self::Steal => "steal",
            Self::Renew => "renew",
            Self::Resign => "resign",
            Self::FencedWrite => "fenced_write",
            Self::Observe => "observe",
            Self::BeliefBegin => "belief_begin",
            Self::BeliefEnd => "belief_end",
            Self::InjectedUnknown => "injected_unknown",
        }
    }

    /// Parse a wire name
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "claim" => Some(Self::Claim),
            "steal" => Some(Self::Steal),
            "renew" => Some(Self::Renew),
            "resign" => Some(Self::Resign),
            "fenced_write" => Some(Self::FencedWrite),
            "observe" => Some(Self::Observe),
            "belief_begin" => Some(Self::BeliefBegin),
            "belief_end" => Some(Self::BeliefEnd),
            "injected_unknown" => Some(Self::InjectedUnknown),
            _ => None,
        }
    }

    /// Whether an op of this kind is allowed to mutate the leader record
    pub fn touches_leader_record(self) -> bool {
        matches!(self, Self::Claim | Self::Steal | Self::Renew | Self::Resign)
    }
}

impl fmt::Display for OpKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What the primitive answered
///
/// A semantic rejection is logged as `Rejected` in a transaction that still
/// commits: a denial that never reaches the log is a failure path the check
/// phase cannot see.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Outcome {
    /// The operation was accepted
    Applied,
    /// The operation was refused by the protocol
    Rejected,
}

impl Outcome {
    /// Whether the operation was accepted
    pub fn is_applied(self) -> bool {
        matches!(self, Self::Applied)
    }
}

/// The `(ballot, generation)` pair the transaction read, and whether the record
/// it came from was vacant
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ObservedIdentity {
    /// Ballot of the record that was read
    pub ballot: u64,
    /// Generation of the record that was read
    pub generation: u64,
    /// Whether the record that was read was a resigned (vacant) one
    pub vacant: bool,
}

// ============================================================================
// RECORD
// ============================================================================

/// The value half of a log entry
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRecord {
    /// What was attempted
    pub op: OpKind,
    /// What the protocol answered
    pub outcome: Outcome,
    /// Identifies one campaign across the retries of a single `db.run`
    pub attempt_id: u64,
    /// The per-term claim token, all-zero when the op carries none
    pub token: [u8; 16],
    /// Ballot the op wrote, or the ballot it acted under
    pub ballot: u64,
    /// Generation the op wrote
    pub generation: u64,
    /// What the transaction read before deciding
    pub observed: Option<ObservedIdentity>,
    /// Whether this transaction actually mutated the leader record
    ///
    /// The single most important field: replay consumes nothing else.
    pub leader_record_written: bool,
    /// Whether this op recognized a write of its own from an earlier,
    /// unacknowledged execution and deliberately wrote nothing
    pub recovery_noop: bool,
    /// Whether a previous execution of this attempt may have committed
    pub maybe_committed: bool,
    /// Whether the attempt was retired because a foreign record had reached the
    /// ballot it wrote
    ///
    /// The other half of the recovery contract: an attempt whose reply was lost
    /// either finds its own record and adopts it (`recovery_noop`) or finds a
    /// stranger at or past its ballot and is spent. Both are resolutions, and
    /// the check phase counts them together.
    pub superseded: bool,
    /// The caller's own (skewed) clock, the only time the recipe ever sees
    pub local_nanos: u64,
    /// True simulated time, sampled inside the transaction after the read
    pub sim_nanos: u64,
    /// When the caller started timing the identity it observed, on its own clock
    pub observation_start_nanos: Option<u64>,
    /// The lease the op advertised, or the one the observed record advertised
    pub lease_nanos: u64,
    /// For belief records: the horizon the driver computed, on its own clock
    pub horizon_nanos: u64,
}

impl LogRecord {
    /// An applied record of `op` with every optional field cleared
    pub fn new(op: OpKind) -> Self {
        Self {
            op,
            outcome: Outcome::Applied,
            attempt_id: 0,
            token: [0u8; 16],
            ballot: 0,
            generation: 0,
            observed: None,
            leader_record_written: false,
            recovery_noop: false,
            maybe_committed: false,
            superseded: false,
            local_nanos: 0,
            sim_nanos: 0,
            observation_start_nanos: None,
            lease_nanos: 0,
            horizon_nanos: 0,
        }
    }

    /// Encode the value half
    pub fn encode(&self) -> Vec<u8> {
        let observed = self.observed.unwrap_or(ObservedIdentity {
            ballot: 0,
            generation: 0,
            vacant: false,
        });
        pack(&(
            LOG_SCHEMA_VERSION,
            self.op.as_str(),
            self.outcome.is_applied(),
            self.attempt_id,
            self.token.as_slice(),
            (
                self.ballot,
                self.generation,
                self.lease_nanos,
                self.horizon_nanos,
            ),
            (
                self.observed.is_some(),
                observed.ballot,
                observed.generation,
                observed.vacant,
            ),
            (
                self.leader_record_written,
                self.recovery_noop,
                self.maybe_committed,
                self.superseded,
            ),
            (
                self.local_nanos,
                self.sim_nanos,
                self.observation_start_nanos.is_some(),
                self.observation_start_nanos.unwrap_or(0),
            ),
        ))
    }

    /// Decode the value half
    ///
    /// # Errors
    ///
    /// [`SchemaError`] if the value is not a record of a known schema version.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        type Encoded = (
            u64,
            String,
            bool,
            u64,
            Vec<u8>,
            (u64, u64, u64, u64),
            (bool, u64, u64, bool),
            (bool, bool, bool, bool),
            (u64, u64, bool, u64),
        );

        let (
            version,
            op,
            applied,
            attempt_id,
            token,
            (ballot, generation, lease_nanos, horizon_nanos),
            (observed_present, observed_ballot, observed_generation, observed_vacant),
            (leader_record_written, recovery_noop, maybe_committed, superseded),
            (local_nanos, sim_nanos, observation_present, observation_start_nanos),
        ): Encoded =
            unpack(bytes).map_err(|e| SchemaError(format!("value is not a log record: {e:?}")))?;

        if version != LOG_SCHEMA_VERSION {
            return Err(SchemaError(format!(
                "unknown log schema version {version}, this build understands {LOG_SCHEMA_VERSION}"
            )));
        }
        let op =
            OpKind::parse(&op).ok_or_else(|| SchemaError(format!("unknown op kind {op:?}")))?;
        let token: [u8; 16] = token
            .try_into()
            .map_err(|_| SchemaError("token is not 16 bytes".to_string()))?;

        Ok(Self {
            op,
            outcome: if applied {
                Outcome::Applied
            } else {
                Outcome::Rejected
            },
            attempt_id,
            token,
            ballot,
            generation,
            observed: observed_present.then_some(ObservedIdentity {
                ballot: observed_ballot,
                generation: observed_generation,
                vacant: observed_vacant,
            }),
            leader_record_written,
            recovery_noop,
            maybe_committed,
            superseded,
            local_nanos,
            sim_nanos,
            observation_start_nanos: observation_present.then_some(observation_start_nanos),
            lease_nanos,
            horizon_nanos,
        })
    }
}

// ============================================================================
// ENTRY
// ============================================================================

/// One log entry: the record plus what its key says about it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    /// Commit versionstamp; entries read in key order are in commit order
    pub versionstamp: [u8; 12],
    /// Which client wrote it
    pub client_id: i32,
    /// The client's own operation counter, unique per client
    pub op_num: u64,
    /// The operation
    pub record: LogRecord,
}

/// The key a logging transaction writes, with an unresolved versionstamp
///
/// Must be written with `MutationType::SetVersionstampedKey`.
pub fn incomplete_log_key(subspace: &Subspace, client_id: i32, op_num: u64) -> Vec<u8> {
    subspace.pack_with_versionstamp(&(Versionstamp::incomplete(0), client_id, op_num))
}

impl LogEntry {
    /// Decode an entry from the key and value the range read returned
    ///
    /// # Errors
    ///
    /// [`SchemaError`] if either half is malformed.
    pub fn decode(subspace: &Subspace, key: &[u8], value: &[u8]) -> Result<Self> {
        let (versionstamp, client_id, op_num): (Versionstamp, i32, u64) = subspace
            .unpack(key)
            .map_err(|e| SchemaError(format!("key is not a log key: {e:?}")))?;

        Ok(Self {
            versionstamp: *versionstamp.as_bytes(),
            client_id,
            op_num,
            record: LogRecord::decode(value)?,
        })
    }
}

// ============================================================================
// TEST FIXTURES
// ============================================================================

#[cfg(test)]
pub(crate) mod fixtures {
    //! Hand-built logs for the invariant tests.
    //!
    //! Every invariant test starts from [`clean_log`] and mutates exactly one
    //! thing, so a test that fails names the property it broke. A helper that
    //! silently repaired a mutation would hand us back the tautologies this
    //! rewrite exists to remove, so the builder does no validation at all: it
    //! writes down whatever the test asks for.

    use super::*;

    /// The lease every fixture advertises
    pub(crate) const LEASE: u64 = 10_000_000_000;
    /// Seconds, in nanoseconds
    pub(crate) const SEC: u64 = 1_000_000_000;

    pub(crate) fn token(byte: u8) -> [u8; 16] {
        [byte; 16]
    }

    /// The leader id the fixtures (and the workload) derive from a client id
    pub(crate) fn leader_id(client_id: i32) -> String {
        format!("process_{client_id}")
    }

    /// Accumulates entries in commit order
    #[derive(Debug, Default)]
    pub(crate) struct LogBuilder {
        entries: Vec<LogEntry>,
        next_op_num: u64,
    }

    impl LogBuilder {
        pub(crate) fn new() -> Self {
            Self::default()
        }

        /// Append `record` as written by `client_id`, returning its index
        pub(crate) fn push(&mut self, client_id: i32, record: LogRecord) -> usize {
            let index = self.entries.len();
            let mut versionstamp = [0u8; 12];
            versionstamp[..8].copy_from_slice(&(index as u64 + 1).to_be_bytes());
            self.entries.push(LogEntry {
                versionstamp,
                client_id,
                op_num: self.next_op_num,
                record,
            });
            self.next_op_num += 1;
            index
        }

        pub(crate) fn entries(&self) -> &[LogEntry] {
            &self.entries
        }

        pub(crate) fn into_entries(self) -> Vec<LogEntry> {
            self.entries
        }
    }

    /// A claim or steal that landed
    pub(crate) fn write(op: OpKind, ballot: u64, generation: u64, tok: u8, sim: u64) -> LogRecord {
        let mut record = LogRecord::new(op);
        record.ballot = ballot;
        record.generation = generation;
        record.token = token(tok);
        record.lease_nanos = LEASE;
        record.leader_record_written = true;
        record.local_nanos = sim;
        record.sim_nanos = sim;
        record.attempt_id = ballot;
        record
    }

    /// A resign, which preserves both halves of the identity
    pub(crate) fn resign(ballot: u64, generation: u64, tok: u8, sim: u64) -> LogRecord {
        let mut record = write(OpKind::Resign, ballot, generation, tok, sim);
        record.lease_nanos = 0;
        record.observed = Some(ObservedIdentity {
            ballot,
            generation,
            vacant: false,
        });
        record
    }

    pub(crate) fn observed(record: &mut LogRecord, ballot: u64, generation: u64, vacant: bool) {
        record.observed = Some(ObservedIdentity {
            ballot,
            generation,
            vacant,
        });
    }

    /// The canonical well-behaved run.
    ///
    /// Client 0 claims an absent record, renews twice, resigns. Client 1
    /// reclaims the vacancy instantly, recovers an unknown commit without
    /// double-claiming, renews, then stops writing (a crash: it never logs a
    /// belief end). Client 2 times the abandoned identity for a full lease,
    /// steals it, and fences a write; client 1 wakes up afterwards and has both
    /// a stale fenced write and a stale renewal rejected.
    pub(crate) fn clean_log() -> Vec<LogEntry> {
        let mut log = LogBuilder::new();

        // -- client 0 takes an absent record at ballot 1 -------------------
        log.push(0, write(OpKind::Claim, 1, 0, 1, SEC));
        log.push(0, belief_begin(0, 1, SEC, 9 * SEC));
        log.push(2, observe(1, 0, false, 2 * SEC));
        log.push(0, write(OpKind::Renew, 1, 1, 1, 3 * SEC));
        log.push(0, fenced_write(1, Outcome::Applied, 3 * SEC + SEC / 2));
        log.push(2, observe(1, 1, false, 4 * SEC));
        {
            // A renewal extends the belief: the driver logs the new horizon.
            let mut extend = belief_begin(0, 1, 5 * SEC, 13 * SEC);
            extend.attempt_id = 2;
            log.push(0, extend);
        }
        log.push(0, write(OpKind::Renew, 1, 2, 1, 5 * SEC));

        // -- client 0 hands the term over cleanly --------------------------
        log.push(0, belief_end(0, 1, 6 * SEC));
        log.push(0, resign(1, 2, 1, 6 * SEC + SEC / 10));

        // -- client 1 reclaims the vacancy with no wait --------------------
        let mut claim = write(OpKind::Claim, 2, 2, 2, 6 * SEC + SEC / 2);
        observed(&mut claim, 1, 2, true);
        log.push(1, claim);

        // The reply to that claim was lost; the retry recognizes its own
        // record and writes nothing.
        let mut recovery = write(OpKind::Claim, 2, 2, 2, 6 * SEC + 6 * SEC / 10);
        recovery.leader_record_written = false;
        recovery.recovery_noop = true;
        recovery.maybe_committed = true;
        observed(&mut recovery, 2, 2, false);
        log.push(1, recovery);

        log.push(
            1,
            belief_begin(1, 2, 6 * SEC + 5 * SEC / 10, 15 * SEC + 4 * SEC / 10),
        );
        log.push(2, observe(2, 2, false, 7 * SEC));
        log.push(1, write(OpKind::Renew, 2, 3, 2, 8 * SEC));

        // -- client 1 stops responding; client 2 times the identity --------
        let mut first_sighting = observe(2, 3, false, 8 * SEC + 2 * SEC / 10);
        first_sighting.observation_start_nanos = Some(8 * SEC + 2 * SEC / 10);
        log.push(2, first_sighting);

        let mut mid_sighting = observe(2, 3, false, 13 * SEC);
        mid_sighting.observation_start_nanos = Some(8 * SEC + 2 * SEC / 10);
        log.push(2, mid_sighting);

        // 10.1 s of continuous observation, one tenth of a second past the
        // lease the abandoned record advertises.
        let mut steal = write(OpKind::Steal, 3, 3, 3, 18 * SEC + 3 * SEC / 10);
        steal.observation_start_nanos = Some(8 * SEC + 2 * SEC / 10);
        observed(&mut steal, 2, 3, false);
        log.push(2, steal);

        log.push(2, belief_begin(2, 3, 18 * SEC + 3 * SEC / 10, 27 * SEC));
        log.push(2, fenced_write(3, Outcome::Applied, 19 * SEC));

        // -- the paused client 1 wakes up and is fenced out ----------------
        log.push(1, fenced_write(2, Outcome::Rejected, 20 * SEC));
        let mut stale_renew = write(OpKind::Renew, 2, 4, 2, 20 * SEC + SEC / 10);
        stale_renew.leader_record_written = false;
        stale_renew.outcome = Outcome::Rejected;
        observed(&mut stale_renew, 3, 3, false);
        log.push(1, stale_renew);

        log.into_entries()
    }

    /// The database state the clean log replays to
    pub(crate) fn clean_snapshot() -> crate::leader_election::replay::ExpectedRecord {
        crate::leader_election::replay::ExpectedRecord {
            ballot: 3,
            generation: 3,
            leader_id: leader_id(2),
            token: token(3),
            lease_nanos: LEASE,
        }
    }

    /// The transitions the recipe's own history subspace should hold
    pub(crate) fn clean_history() -> Vec<crate::leader_election::invariants::HistoryEntry> {
        use crate::leader_election::invariants::{HistoryEntry, HistoryKind};
        vec![
            HistoryEntry {
                kind: HistoryKind::Claim,
                ballot: 1,
                leader_id: leader_id(0),
            },
            HistoryEntry {
                kind: HistoryKind::Resign,
                ballot: 1,
                leader_id: leader_id(0),
            },
            HistoryEntry {
                kind: HistoryKind::Claim,
                ballot: 2,
                leader_id: leader_id(1),
            },
            HistoryEntry {
                kind: HistoryKind::Steal,
                ballot: 3,
                leader_id: leader_id(2),
            },
        ]
    }

    /// The marker a client writes when it throws a claim reply away
    pub(crate) fn injected_unknown(ballot: u64, tok: u8, sim: u64) -> LogRecord {
        let mut record = LogRecord::new(OpKind::InjectedUnknown);
        record.ballot = ballot;
        record.token = token(tok);
        record.attempt_id = ballot;
        record.maybe_committed = true;
        record.local_nanos = sim;
        record.sim_nanos = sim;
        record
    }

    /// A re-probe that found a stranger at or past the ballot it wrote
    ///
    /// The terminal half of the recovery contract: nothing was written, the
    /// outcome is a refusal, and the token is spent from here on.
    pub(crate) fn superseded(observed_ballot: u64, tok: u8, sim: u64) -> LogRecord {
        let mut record = LogRecord::new(OpKind::Steal);
        record.outcome = Outcome::Rejected;
        record.token = token(tok);
        record.attempt_id = u64::from(tok);
        record.superseded = true;
        record.maybe_committed = true;
        record.lease_nanos = LEASE;
        record.local_nanos = sim;
        record.sim_nanos = sim;
        observed(&mut record, observed_ballot, 1, false);
        record
    }

    /// A run in which the driver forced two unknown commits.
    ///
    /// Both halves of the recovery contract, in one log. Client 0 claims,
    /// throws the reply away, and its re-probe finds its own record and adopts
    /// it. Client 1 claims the vacancy it leaves, throws that reply away too,
    /// and is stolen from while it waits: its re-probe finds a stranger at a
    /// higher ballot, retires the token, and the campaign that follows uses a
    /// fresh one.
    ///
    /// A sibling of [`clean_log`] rather than an extension of it, because many
    /// tests index into that fixture by position.
    pub(crate) fn injected_recovery_log() -> Vec<LogEntry> {
        let mut log = LogBuilder::new();

        // -- client 0 claims, drops the reply, and adopts its own record ----
        log.push(0, write(OpKind::Claim, 1, 0, 1, SEC));
        log.push(0, injected_unknown(1, 1, SEC + SEC / 10));

        let mut adopted = write(OpKind::Claim, 1, 0, 1, SEC + 5 * SEC / 10);
        adopted.leader_record_written = false;
        adopted.recovery_noop = true;
        adopted.maybe_committed = true;
        observed(&mut adopted, 1, 0, false);
        log.push(0, adopted);

        log.push(0, belief_begin(0, 1, SEC + 5 * SEC / 10, 9 * SEC));
        log.push(2, observe(1, 0, false, 2 * SEC));
        log.push(0, write(OpKind::Renew, 1, 1, 1, 3 * SEC));
        log.push(2, observe(1, 1, false, 4 * SEC));
        log.push(0, belief_end(0, 1, 5 * SEC));
        log.push(0, resign(1, 1, 1, 5 * SEC + SEC / 10));

        // -- client 1 takes the vacancy and drops that reply too ------------
        let mut claim = write(OpKind::Claim, 2, 1, 2, 5 * SEC + 5 * SEC / 10);
        observed(&mut claim, 1, 1, true);
        log.push(1, claim);
        log.push(1, injected_unknown(2, 2, 5 * SEC + 6 * SEC / 10));
        // No belief: a client that never heard back never starts believing,
        // which is what makes the injection the same shape as a crash.

        // -- client 2 times the abandoned identity and takes it -------------
        let mut first_sighting = observe(2, 1, false, 5 * SEC + 8 * SEC / 10);
        first_sighting.observation_start_nanos = Some(5 * SEC + 8 * SEC / 10);
        log.push(2, first_sighting);

        let mut mid_sighting = observe(2, 1, false, 12 * SEC);
        mid_sighting.observation_start_nanos = Some(5 * SEC + 8 * SEC / 10);
        log.push(2, mid_sighting);

        let mut steal = write(OpKind::Steal, 3, 1, 3, 16 * SEC);
        steal.observation_start_nanos = Some(5 * SEC + 8 * SEC / 10);
        observed(&mut steal, 2, 1, false);
        log.push(2, steal);
        log.push(2, belief_begin(2, 3, 16 * SEC, 25 * SEC));

        // -- client 1 re-probes and finds the stranger ----------------------
        log.push(1, superseded(3, 2, 17 * SEC));

        log.push(2, write(OpKind::Renew, 3, 2, 3, 18 * SEC));
        log.push(2, belief_end(2, 3, 20 * SEC));
        log.push(2, resign(3, 2, 3, 20 * SEC + SEC / 10));

        // -- and campaigns again under a token the retirement did not spend -
        let mut fresh = write(OpKind::Claim, 4, 2, 4, 20 * SEC + 5 * SEC / 10);
        observed(&mut fresh, 3, 2, true);
        log.push(1, fresh);
        let mut belief = belief_begin(1, 4, 20 * SEC + 5 * SEC / 10, 29 * SEC);
        belief.token = token(4);
        log.push(1, belief);

        log.into_entries()
    }

    /// The database state [`injected_recovery_log`] replays to
    pub(crate) fn injected_recovery_snapshot() -> crate::leader_election::replay::ExpectedRecord {
        crate::leader_election::replay::ExpectedRecord {
            ballot: 4,
            generation: 2,
            leader_id: leader_id(1),
            token: token(4),
            lease_nanos: LEASE,
        }
    }

    /// The transitions [`injected_recovery_log`] should have left in the
    /// recipe's own history subspace
    pub(crate) fn injected_recovery_history()
    -> Vec<crate::leader_election::invariants::HistoryEntry> {
        use crate::leader_election::invariants::{HistoryEntry, HistoryKind};
        let entry = |kind, ballot, client| HistoryEntry {
            kind,
            ballot,
            leader_id: leader_id(client),
        };
        vec![
            entry(HistoryKind::Claim, 1, 0),
            entry(HistoryKind::Resign, 1, 0),
            entry(HistoryKind::Claim, 2, 1),
            entry(HistoryKind::Steal, 3, 2),
            entry(HistoryKind::Resign, 3, 2),
            entry(HistoryKind::Claim, 4, 1),
        ]
    }

    pub(crate) fn observe(ballot: u64, generation: u64, vacant: bool, sim: u64) -> LogRecord {
        let mut record = LogRecord::new(OpKind::Observe);
        record.local_nanos = sim;
        record.sim_nanos = sim;
        record.lease_nanos = if vacant { 0 } else { LEASE };
        observed(&mut record, ballot, generation, vacant);
        record
    }

    pub(crate) fn fenced_write(ballot: u64, outcome: Outcome, sim: u64) -> LogRecord {
        let mut record = LogRecord::new(OpKind::FencedWrite);
        record.ballot = ballot;
        record.outcome = outcome;
        record.local_nanos = sim;
        record.sim_nanos = sim;
        record
    }

    pub(crate) fn belief_begin(client_id: i32, ballot: u64, sim: u64, horizon: u64) -> LogRecord {
        let mut record = LogRecord::new(OpKind::BeliefBegin);
        record.ballot = ballot;
        record.token = token(client_id as u8 + 1);
        record.local_nanos = sim;
        record.sim_nanos = sim;
        record.horizon_nanos = horizon;
        record.lease_nanos = LEASE;
        record
    }

    pub(crate) fn belief_end(client_id: i32, ballot: u64, sim: u64) -> LogRecord {
        let mut record = LogRecord::new(OpKind::BeliefEnd);
        record.ballot = ballot;
        record.token = token(client_id as u8 + 1);
        record.local_nanos = sim;
        record.sim_nanos = sim;
        record
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    fn sample() -> LogRecord {
        let mut record = LogRecord::new(OpKind::Steal);
        record.outcome = Outcome::Rejected;
        record.attempt_id = 17;
        record.token = token(9);
        record.ballot = 42;
        record.generation = 7;
        record.observed = Some(ObservedIdentity {
            ballot: 41,
            generation: 3,
            vacant: true,
        });
        record.leader_record_written = true;
        record.recovery_noop = true;
        record.maybe_committed = true;
        record.superseded = true;
        record.local_nanos = 123_456_789;
        record.sim_nanos = 987_654_321;
        record.observation_start_nanos = Some(555);
        record.lease_nanos = LEASE;
        record.horizon_nanos = 42 * SEC;
        record
    }

    #[test]
    fn every_field_survives_a_round_trip() {
        let record = sample();
        assert_eq!(LogRecord::decode(&record.encode()).unwrap(), record);
    }

    #[test]
    fn an_absent_observation_stays_absent() {
        // `Some(0)` and `None` mean very different things to the observation
        // discipline check, so the encoding must not collapse them.
        let mut record = LogRecord::new(OpKind::Claim);
        record.observed = None;
        record.observation_start_nanos = None;
        let decoded = LogRecord::decode(&record.encode()).unwrap();
        assert_eq!(decoded.observed, None);
        assert_eq!(decoded.observation_start_nanos, None);

        record.observation_start_nanos = Some(0);
        record.observed = Some(ObservedIdentity {
            ballot: 0,
            generation: 0,
            vacant: false,
        });
        let decoded = LogRecord::decode(&record.encode()).unwrap();
        assert_eq!(decoded.observation_start_nanos, Some(0));
        assert!(decoded.observed.is_some());
    }

    #[test]
    fn every_op_kind_round_trips_through_its_wire_name() {
        for op in [
            OpKind::Claim,
            OpKind::Steal,
            OpKind::Renew,
            OpKind::Resign,
            OpKind::FencedWrite,
            OpKind::Observe,
            OpKind::BeliefBegin,
            OpKind::BeliefEnd,
            OpKind::InjectedUnknown,
        ] {
            assert_eq!(OpKind::parse(op.as_str()), Some(op));
        }
        assert_eq!(OpKind::parse("nonsense"), None);
    }

    #[test]
    fn an_injected_unknown_marker_may_not_touch_the_leader_record() {
        // The marker is written in a transaction of its own, after the claim it
        // describes committed. Nothing about it is a transition, and replay
        // treats an op that says otherwise as an anomaly.
        assert!(!OpKind::InjectedUnknown.touches_leader_record());
        for op in [OpKind::Claim, OpKind::Steal, OpKind::Renew, OpKind::Resign] {
            assert!(op.touches_leader_record());
        }
    }

    #[test]
    fn a_key_round_trips_and_orders_by_commit() {
        let subspace = log_subspace();
        // The incomplete versionstamp is what the mutation resolves; a decoded
        // key must still yield the client and op number the writer put there.
        let key = incomplete_log_key(&subspace, 3, 17);
        let mut resolved = key.clone();
        // Strip the 4-byte offset suffix the versionstamp mutation consumes.
        resolved.truncate(resolved.len() - 4);
        // Fill the incomplete stamp with a plausible commit version.
        let stamp_start = resolved.len() - 12 - 2 - 1;
        resolved[stamp_start..stamp_start + 10].copy_from_slice(&[7u8; 10]);

        let record = LogRecord::new(OpKind::Observe);
        let entry = LogEntry::decode(&subspace, &resolved, &record.encode()).unwrap();
        assert_eq!(entry.client_id, 3);
        assert_eq!(entry.op_num, 17);
        assert_eq!(entry.record.op, OpKind::Observe);
    }

    #[test]
    fn malformed_values_are_rejected_loudly() {
        assert!(LogRecord::decode(&[]).is_err());
        assert!(LogRecord::decode(b"not a tuple at all").is_err());

        // A future schema version must not be read as if it were this one.
        let mut bytes = LogRecord::new(OpKind::Claim).encode();
        // The version is the first packed element: a 1-byte integer code plus
        // its payload. Bumping the payload bumps the version.
        let version_byte = bytes
            .iter()
            .position(|b| *b == LOG_SCHEMA_VERSION as u8)
            .expect("the schema version is encoded verbatim");
        bytes[version_byte] = 9;
        assert!(LogRecord::decode(&bytes).is_err());
    }

    #[test]
    fn the_injected_recovery_fixture_carries_both_resolutions() {
        let entries = injected_recovery_log();
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.record.op == OpKind::InjectedUnknown)
                .count(),
            2
        );
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.record.recovery_noop)
                .count(),
            1
        );
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.record.superseded)
                .count(),
            1
        );
        for entry in &entries {
            let decoded = LogRecord::decode(&entry.record.encode()).unwrap();
            assert_eq!(decoded, entry.record);
        }
    }

    #[test]
    fn the_clean_fixture_is_ordered_and_decodable() {
        let entries = clean_log();
        assert!(entries.len() > 15, "the fixture should be a real run");
        for entry in &entries {
            let decoded = LogRecord::decode(&entry.record.encode()).unwrap();
            assert_eq!(decoded, entry.record);
        }
        // Versionstamps stand in for commit order in the fixtures.
        for pair in entries.windows(2) {
            assert!(pair[0].versionstamp < pair[1].versionstamp);
        }
    }
}
