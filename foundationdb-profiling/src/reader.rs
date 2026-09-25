//! Paged, resumable reading of the client profiling keyspace.
//!
//! Every sampled transaction is stored as one or more chunks under
//! `\xff\x02/fdbClientInfo/client_latency/`. A chunk key is laid out as:
//!
//! ```text
//! PREFIX | versionstamp (10) | '/' | transaction id (16) | '/' | chunk (4, BE) | total (4, BE) | '/' | user id ...
//! ```
//!
//! [`ProfileScanner::read_page`] reads a bounded range of these keys, reassembles the
//! chunks of each transaction, decodes them with [`decode_events`] and returns a
//! [`Page`] with a [`Cursor`] to resume from.

use crate::decode::{DecodeError, decode_events};
use crate::event::{Event, ProtocolVersion};
use foundationdb::{BudgetExceeded, ClientBudget, FdbError, KeySelector, RangeOption, Transaction};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;
use tracing::instrument;

/// Prefix of the client profiling keyspace.
///
/// Defined by the [C++ client](https://github.com/apple/foundationdb/blob/main/contrib/transaction_profiling_analyzer/transaction_profiling_analyzer.py#L421).
pub const PROFILE_PREFIX: &[u8; 32] = b"\xff\x02/fdbClientInfo/client_latency/";

/// Exclusive end of the client profiling keyspace (`strinc(PROFILE_PREFIX)`).
const PROFILE_END: &[u8; 32] = b"\xff\x02/fdbClientInfo/client_latency0";

const VERSIONSTAMP_LEN: usize = 10;
const ID_LEN: usize = 16;
const VERSIONSTAMP_START: usize = PROFILE_PREFIX.len();
const VERSIONSTAMP_END: usize = VERSIONSTAMP_START + VERSIONSTAMP_LEN;
const ID_START: usize = VERSIONSTAMP_END + 1;
const ID_END: usize = ID_START + ID_LEN;
const CHUNK_START: usize = ID_END + 1;
const TOTAL_START: usize = CHUNK_START + 4;
/// Shortest key [`ProfileScanner::read_page`] can parse: everything up to the total chunk
/// count.
const MIN_KEY_LEN: usize = TOTAL_START + 4;

/// Default [`ProfileScanner::max_pending_versions`]: a pending transaction is dropped as
/// broken once a row more than this many versions (about 10 seconds) after its first
/// chunk was fed without completing it. The client writes the rest of a transaction in
/// its next commit, right after the first one.
const DEFAULT_MAX_PENDING_VERSIONS: i64 = 10_000_000;

/// Default [`ProfileScanner::max_pending_bytes`]: a pending transaction is dropped as
/// broken once more than this many key and value bytes were fed since its first chunk
/// (inclusive) without completing it. The client splits a flush in commits of 0.8 times
/// the transaction size limit (10 MB), so the continuation of a transaction is at most
/// about two commits away from its first chunk.
const DEFAULT_MAX_PENDING_BYTES: u64 = 16 * 1024 * 1024;

/// Version of the [`Cursor`] serialization.
const CURSOR_FORMAT: u8 = 1;
/// Serialized cursor header: format byte and the big-endian `u32` length of `resume`.
const CURSOR_HEADER_LEN: usize = 5;

/// Opaque, resumable and persistable position in the profiling keyspace.
///
/// A cursor holds two keys: where the next range read begins, and how far the previous
/// pages already reported transactions. The first one can lag behind the second while a
/// transaction whose chunks were written by several commits is not complete yet: the
/// next page reads again from its first chunk and does not report twice what earlier
/// pages already returned.
///
/// Persist it with [`Cursor::as_bytes`] and restore it with [`Cursor::from_bytes`] to
/// resume reading later, for instance to tail the keyspace.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Cursor {
    /// `CURSOR_FORMAT | len(resume) (u32, BE) | resume | emitted_up_to`.
    ///
    /// `resume` is the inclusive begin key of the next read, `emitted_up_to` the key
    /// right after the last row read by the previous pages. Both start with
    /// [`PROFILE_PREFIX`] and `resume <= emitted_up_to`.
    bytes: Vec<u8>,
}

/// Bytes given to [`Cursor::from_bytes`] are not a serialized [`Cursor`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid profiling cursor")]
pub struct InvalidCursor;

impl Cursor {
    /// Cursor at the beginning of the profiling keyspace.
    #[instrument(level = "trace")]
    pub fn beginning() -> Self {
        Cursor::at_key(PROFILE_PREFIX)
    }

    /// Cursor at the first record with a commit version greater than or equal to
    /// `version`.
    ///
    /// The version is the one at which the profiling record was written by the client,
    /// which is some time (up to the client's flush interval) after the profiled
    /// transaction ran. Negative versions are treated as 0.
    #[instrument(level = "trace")]
    pub fn at_version(version: i64) -> Self {
        Cursor::at_key(&version_key(version))
    }

    /// Serialized form of the cursor, to persist it.
    #[instrument(level = "trace", skip_all)]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Restores a cursor persisted with [`Cursor::as_bytes`].
    ///
    /// # Errors
    ///
    /// Returns [`InvalidCursor`] when `bytes` is not a serialized cursor.
    #[instrument(level = "trace", skip_all, fields(len = bytes.len()))]
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, InvalidCursor> {
        let cursor = Cursor { bytes };
        let (resume, emitted_up_to) = cursor.try_split().ok_or(InvalidCursor)?;
        if resume.starts_with(PROFILE_PREFIX)
            && emitted_up_to.starts_with(PROFILE_PREFIX)
            && resume <= emitted_up_to
        {
            Ok(cursor)
        } else {
            Err(InvalidCursor)
        }
    }

    /// Both keys at `key`.
    fn at_key(key: &[u8]) -> Self {
        Cursor::new(key, key)
    }

    fn new(resume: &[u8], emitted_up_to: &[u8]) -> Self {
        let mut bytes = Vec::with_capacity(CURSOR_HEADER_LEN + resume.len() + emitted_up_to.len());
        bytes.push(CURSOR_FORMAT);
        // keys are bounded by the FoundationDB key size limit, far below u32::MAX
        bytes.extend_from_slice(&(resume.len() as u32).to_be_bytes());
        bytes.extend_from_slice(resume);
        bytes.extend_from_slice(emitted_up_to);
        Cursor { bytes }
    }

    fn try_split(&self) -> Option<(&[u8], &[u8])> {
        let (&format, rest) = self.bytes.split_first()?;
        if format != CURSOR_FORMAT {
            return None;
        }
        let len = rest.get(..4)?;
        let len = u32::from_be_bytes([len[0], len[1], len[2], len[3]]) as usize;
        let rest = &rest[4..];
        if len > rest.len() {
            return None;
        }
        Some(rest.split_at(len))
    }

    /// Inclusive begin key of the next read.
    fn resume(&self) -> &[u8] {
        self.try_split()
            .map_or(PROFILE_PREFIX, |(resume, _)| resume)
    }

    /// Outcomes produced by rows before this key were reported by earlier pages.
    fn emitted_up_to(&self) -> &[u8] {
        self.try_split()
            .map_or(PROFILE_PREFIX, |(_, emitted)| emitted)
    }
}

/// `PROFILE_PREFIX | version (8, BE) | \x00\x00`: the first key of that version.
fn version_key(version: i64) -> Vec<u8> {
    let mut key = PROFILE_PREFIX.to_vec();
    key.extend_from_slice(&version.max(0).to_be_bytes());
    key.extend_from_slice(b"\x00\x00");
    key
}

/// Reads pages of profiling data. Build it once and reuse it for every page of a scan:
/// its reassembly limits must stay the same across the pages that share a cursor.
#[derive(Debug, Clone)]
pub struct ProfileScanner {
    budget: ClientBudget,
    max_transactions: usize,
    end_version: Option<i64>,
    max_pending_versions: i64,
    max_pending_bytes: u64,
}

impl Default for ProfileScanner {
    fn default() -> Self {
        ProfileScanner {
            budget: ClientBudget {
                time_limit: Some(Duration::from_secs(2)),
                ..ClientBudget::default()
            },
            max_transactions: 1_000,
            end_version: None,
            max_pending_versions: DEFAULT_MAX_PENDING_VERSIONS,
            max_pending_bytes: DEFAULT_MAX_PENDING_BYTES,
        }
    }
}

impl ProfileScanner {
    /// A scanner with the default configuration: a 2 second wall-clock [`budget`](Self::budget),
    /// [`max_transactions`](Self::max_transactions) of 1,000, no [`end_version`](Self::end_version)
    /// and the default reassembly limits ([`max_pending_versions`](Self::max_pending_versions),
    /// [`max_pending_bytes`](Self::max_pending_bytes)).
    #[instrument(level = "trace")]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the whole [`ClientBudget`] bounding [`read_page`](Self::read_page).
    ///
    /// Use this to set byte limits (`max_bytes_read`, `max_bytes_written`) in addition to
    /// or instead of the default time limit, or to give the budget a
    /// [`Clock`](foundationdb::Clock) with [`ClientBudget::with_clock`] so its time limit
    /// is measured deterministically, for instance under simulation.
    ///
    /// [`read_page`](Self::read_page) applies this budget by calling
    /// `trx.set_client_budget(...)`, which replaces any budget the caller may have set on
    /// that transaction directly.
    #[instrument(level = "trace", skip_all)]
    pub fn budget(self, budget: ClientBudget) -> Self {
        ProfileScanner { budget, ..self }
    }

    /// Maximum number of transactions to reassemble in one page, counting both
    /// [`Page::transactions`] and transactions skipped for a decode error. 0 reads
    /// nothing. Defaults to 1,000.
    #[instrument(level = "trace", skip_all)]
    pub fn max_transactions(self, n: usize) -> Self {
        ProfileScanner {
            max_transactions: n,
            ..self
        }
    }

    /// Exclusive upper bound on the record commit version read by [`read_page`](Self::read_page).
    /// Unset (the default) reads to the end of the keyspace.
    #[instrument(level = "trace", skip_all)]
    pub fn end_version(self, version: i64) -> Self {
        ProfileScanner {
            end_version: Some(version),
            ..self
        }
    }

    /// Bounds how long a multi-chunk transaction may stay incomplete before its chunks
    /// are dropped and reported as [`SkipReason::BrokenChunks`], in versions elapsed
    /// since its first chunk was read (about 10,000,000 versions is about 10 seconds).
    /// Defaults to `10_000_000`.
    ///
    /// The client writes the rest of a multi-chunk transaction in its next commit, right
    /// after the first one, so a transaction is normally reassembled well within the
    /// default.
    ///
    /// Changing this between pages of a scan that share a cursor can return a
    /// transaction twice or lose it: keep it the same for every page of a scan.
    #[instrument(level = "trace", skip_all)]
    pub fn max_pending_versions(self, versions: i64) -> Self {
        ProfileScanner {
            max_pending_versions: versions,
            ..self
        }
    }

    /// Bounds how many key and value bytes may be read for a pending multi-chunk
    /// transaction, measured from its first chunk (inclusive), before its chunks are
    /// dropped and reported as [`SkipReason::BrokenChunks`]. Defaults to `16 MiB`.
    ///
    /// The C++ client splits a flush into commits of 0.8 times `TRANSACTION_SIZE_LIMIT`
    /// (a client knob, `1e7` by default), so the continuation of a transaction is at
    /// most about two commits away from its first chunk: raise this (to about twice the
    /// commit size) when a deployment raises that knob.
    ///
    /// Changing this between pages of a scan that share a cursor can return a
    /// transaction twice or lose it: keep it the same for every page of a scan.
    #[instrument(level = "trace", skip_all)]
    pub fn max_pending_bytes(self, bytes: u64) -> Self {
        ProfileScanner {
            max_pending_bytes: bytes,
            ..self
        }
    }
}

/// One sampled transaction, reassembled and decoded.
#[derive(Debug, Clone, PartialEq)]
pub struct ProfiledTransaction {
    /// Commit version of the profiling record (the first 8 bytes of `versionstamp`).
    pub version: i64,
    /// Versionstamp of the first chunk of the profiling record. The client can write the
    /// chunks of one transaction in several commits, the later ones then carry greater
    /// versionstamps.
    pub versionstamp: [u8; 10],
    /// Transaction id chosen by the client that sampled the transaction.
    pub id: [u8; 16],
    /// Protocol version of the client that wrote the record.
    pub protocol_version: ProtocolVersion,
    /// Events of the transaction, in the order the client recorded them.
    pub events: Vec<Event>,
}

/// A transaction that could not be returned.
#[derive(Debug, Clone, PartialEq)]
pub struct Skipped {
    /// Versionstamp of the first chunk read of the profiling record.
    pub versionstamp: [u8; 10],
    /// Transaction id chosen by the client that sampled the transaction.
    pub id: [u8; 16],
    /// Why it was skipped.
    pub reason: SkipReason,
}

/// Why a transaction was [`Skipped`].
#[derive(Debug, Clone, PartialEq)]
pub enum SkipReason {
    /// All chunks were read but the reassembled blob did not decode.
    Decode(DecodeError),
    /// Chunks are missing, out of order or inconsistent, so the blob could not be
    /// reassembled.
    BrokenChunks,
}

/// Why [`ProfileScanner::read_page`] stopped before the end of its range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// The transaction's [`ClientBudget`](foundationdb::ClientBudget) was exceeded.
    Budget(BudgetExceeded),
    /// [`ProfileScanner::max_transactions`] transactions were reassembled.
    MaxTransactions,
}

/// Result of [`ProfileScanner::read_page`].
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    /// Decoded transactions, in the order they completed (the key of their last chunk).
    pub transactions: Vec<ProfiledTransaction>,
    /// Transactions that could not be returned, in the order they were detected.
    pub skipped: Vec<Skipped>,
    /// Where to resume. Always valid, also to tail the keyspace once `exhausted`.
    pub next: Cursor,
    /// Whether the end of the range (`end_version` or the end of the keyspace) was
    /// reached.
    pub exhausted: bool,
    /// Why the page stopped early, `None` when `exhausted`.
    pub stopped_by: Option<StopReason>,
}

impl ProfileScanner {
    /// Reads one bounded page of profiled transactions, starting at `cursor`.
    ///
    /// This first calls `trx.set_client_budget(...)` with this scanner's
    /// [`budget`](Self::budget), **replacing** any budget the caller may have set on
    /// `trx` directly: this starts a fresh accounting generation, so a retried
    /// `db.run` attempt always gets the full allowance again. This crate still sets no
    /// FoundationDB transaction option: the caller must set
    /// `TransactionOption::ReadSystemKeys` (and `ReadLockAware` on a locked cluster)
    /// itself.
    ///
    /// The range is read with snapshot reads, in batches ([`StreamingMode::Iterator`]).
    /// After each batch the budget is checked with [`Transaction::check_client_budget`]:
    /// when exceeded, the page stops with [`StopReason::Budget`]. The check happens
    /// *between* batches, never inside one, so expect an overshoot of up to one batch.
    /// To guarantee progress, the budget only stops a page once it read past what the
    /// previous pages read, so a page may overshoot further still. Unless the budget
    /// carries a [`Clock`](foundationdb::Clock) (see [`ClientBudget::with_clock`]), its
    /// time limit is measured with the wall clock, which is not reproducible under
    /// simulation: give it a simulated clock there instead. The page also stops once
    /// [`max_transactions`](Self::max_transactions) transactions are reassembled.
    ///
    /// Each transaction is returned once over a sequence of pages that each resume from
    /// the previous [`Page::next`]. The client may write the chunks of one transaction
    /// in several commits (when a flush exceeds the transaction size limit), with rows
    /// of other transactions in between: such a transaction is reassembled across them,
    /// and while it is incomplete [`Page::next`] keeps pointing at its first chunk, so
    /// the next page reads it again in full. A transaction still incomplete once it
    /// exceeds [`max_pending_versions`](Self::max_pending_versions) or
    /// [`max_pending_bytes`](Self::max_pending_bytes) since its first chunk is reported
    /// as [`SkipReason::BrokenChunks`], as is a chunk whose earlier chunks are missing,
    /// including chunks of a transaction that began before the cursor the scan started
    /// from ([`Cursor::beginning`], [`Cursor::at_version`]). A broken transaction whose
    /// chunks span a page boundary can be reported by both pages.
    ///
    /// These bounds also bound what a page reads again: [`Page::next`] lags behind the
    /// rows already read by at most [`max_pending_bytes`](Self::max_pending_bytes) plus
    /// one batch, which a page can read again well within the 5 second transaction
    /// lifetime, even when its budget would stop it earlier.
    ///
    /// A transaction whose later chunks lie beyond [`end_version`](Self::end_version)
    /// stays incomplete: the page is `exhausted` and [`Page::next`] points at its first
    /// chunk, so a bounded read never returns it but an unbounded read from that cursor
    /// does.
    ///
    /// A transaction that fails to decode is reported in [`Page::skipped`] and does not
    /// fail the page.
    ///
    /// # Errors
    ///
    /// Only FoundationDB errors, which the caller's retry loop should handle.
    ///
    /// [`StreamingMode::Iterator`]: foundationdb::options::StreamingMode::Iterator
    #[instrument(
        level = "debug",
        skip_all,
        fields(
            max_transactions = self.max_transactions,
            end_version = ?self.end_version,
            transactions,
            skipped,
            exhausted,
        )
    )]
    pub async fn read_page(&self, trx: &Transaction, cursor: &Cursor) -> Result<Page, FdbError> {
        trx.set_client_budget(self.budget.clone());

        let begin = cursor.resume();
        let end = match self.end_version {
            Some(version) => version_key(version),
            None => PROFILE_END.to_vec(),
        };
        let mut page = PageBuilder::new(
            cursor,
            self.max_transactions,
            self.max_pending_versions,
            self.max_pending_bytes,
        );

        if self.max_transactions == 0 {
            return Ok(page.finish(Some(StopReason::MaxTransactions)));
        }
        if begin >= end.as_slice() {
            return Ok(page.finish(None));
        }

        let mut range = RangeOption::from((
            KeySelector::first_greater_or_equal(begin),
            KeySelector::first_greater_or_equal(end.as_slice()),
        ));
        let mut iteration = 1;
        loop {
            let batch = trx.get_range(&range, iteration, true).await?;
            iteration += 1;
            let next = range.next_range(&batch);
            let rows = batch.iter().map(|kv| (kv.key(), kv.value()));
            if let Some(stopped_by) =
                page.feed_batch(rows, next.is_some(), || trx.check_client_budget())
            {
                return Ok(page.finish(stopped_by));
            }
            match next {
                Some(next) => range = next,
                None => return Ok(page.finish(None)),
            }
        }
    }
}

/// Accumulates the output of the [`Assembler`] into a [`Page`].
struct PageBuilder {
    assembler: Assembler,
    /// The cursor the page started from.
    start: Cursor,
    transactions: Vec<ProfiledTransaction>,
    skipped: Vec<Skipped>,
    reassembled: usize,
    max_transactions: usize,
    out: Vec<Assembled>,
}

impl PageBuilder {
    fn new(
        start: &Cursor,
        max_transactions: usize,
        max_pending_versions: i64,
        max_pending_bytes: u64,
    ) -> Self {
        PageBuilder {
            assembler: Assembler::new(
                start.emitted_up_to().to_vec(),
                max_pending_versions,
                max_pending_bytes,
            ),
            start: start.clone(),
            transactions: Vec::new(),
            skipped: Vec::new(),
            reassembled: 0,
            max_transactions,
            out: Vec::new(),
        }
    }

    /// Feeds one batch of rows. Returns `Some(stopped_by)` when the page is done: `more`
    /// tells whether the range has rows after this batch, `check_budget` is called after a
    /// batch that is not the last one.
    fn feed_batch<'a>(
        &mut self,
        rows: impl IntoIterator<Item = (&'a [u8], &'a [u8])>,
        more: bool,
        check_budget: impl FnOnce() -> Result<(), BudgetExceeded>,
    ) -> Option<Option<StopReason>> {
        for (key, value) in rows {
            if self.push(key, value) {
                return Some(Some(StopReason::MaxTransactions));
            }
        }
        if !more {
            return Some(None);
        }
        match check_budget() {
            Err(exceeded) if self.progressed() => Some(Some(StopReason::Budget(exceeded))),
            _ => None,
        }
    }

    /// Feeds one row. Returns true once `max_transactions` transactions are reassembled.
    fn push(&mut self, key: &[u8], value: &[u8]) -> bool {
        self.assembler.push(key, value, &mut self.out);
        for assembled in self.out.drain(..) {
            match assembled {
                Assembled::Complete {
                    versionstamp,
                    id,
                    blob,
                } => {
                    self.reassembled += 1;
                    match decode_events(&blob) {
                        Ok((protocol_version, events)) => {
                            self.transactions.push(ProfiledTransaction {
                                version: version_of(&versionstamp),
                                versionstamp,
                                id,
                                protocol_version,
                                events,
                            })
                        }
                        Err(err) => {
                            tracing::debug!(error = %err, "skipping undecodable transaction");
                            self.skipped.push(Skipped {
                                versionstamp,
                                id,
                                reason: SkipReason::Decode(err),
                            })
                        }
                    }
                }
                Assembled::Broken { versionstamp, id } => {
                    tracing::debug!("skipping transaction with broken chunks");
                    self.skipped.push(Skipped {
                        versionstamp,
                        id,
                        reason: SkipReason::BrokenChunks,
                    })
                }
            }
        }
        self.reassembled >= self.max_transactions
    }

    /// Whether this page read past what the previous pages read, so that its
    /// `emitted_up_to` advances.
    fn progressed(&self) -> bool {
        self.assembler
            .last_key
            .as_deref()
            .is_some_and(|last| last >= self.start.emitted_up_to())
    }

    fn finish(self, stopped_by: Option<StopReason>) -> Page {
        let span = tracing::Span::current();
        span.record("transactions", self.transactions.len());
        span.record("skipped", self.skipped.len());
        span.record("exhausted", stopped_by.is_none());
        let next = match &self.assembler.last_key {
            None => self.start.clone(),
            Some(last) => {
                let after_last = [last.as_slice(), b"\x00"].concat();
                let resume = self.assembler.pending_first_key().unwrap_or(&after_last);
                let emitted_up_to = self.start.emitted_up_to().max(after_last.as_slice());
                Cursor::new(resume, emitted_up_to)
            }
        };
        Page {
            next,
            transactions: self.transactions,
            skipped: self.skipped,
            exhausted: stopped_by.is_none(),
            stopped_by,
        }
    }
}

fn version_of(versionstamp: &[u8; 10]) -> i64 {
    let mut version = [0u8; 8];
    version.copy_from_slice(&versionstamp[..8]);
    i64::from_be_bytes(version)
}

/// The fields of a chunk key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ChunkKey {
    versionstamp: [u8; 10],
    id: [u8; 16],
    chunk: u32,
    total: u32,
}

impl ChunkKey {
    /// Parses a chunk key by offsets, ignoring what follows the total chunk count.
    /// Returns `None` for a key that is too short or has misplaced separators.
    fn parse(key: &[u8]) -> Option<Self> {
        if key.len() < MIN_KEY_LEN
            || !key.starts_with(PROFILE_PREFIX)
            || key[VERSIONSTAMP_END] != b'/'
            || key[ID_END] != b'/'
        {
            return None;
        }
        let mut versionstamp = [0u8; VERSIONSTAMP_LEN];
        versionstamp.copy_from_slice(&key[VERSIONSTAMP_START..VERSIONSTAMP_END]);
        let mut id = [0u8; ID_LEN];
        id.copy_from_slice(&key[ID_START..ID_END]);
        let be = |start: usize| {
            let mut n = [0u8; 4];
            n.copy_from_slice(&key[start..start + 4]);
            u32::from_be_bytes(n)
        };
        Some(ChunkKey {
            versionstamp,
            id,
            chunk: be(CHUNK_START),
            total: be(TOTAL_START),
        })
    }
}

/// Output of the [`Assembler`].
#[derive(Debug, Clone, PartialEq, Eq)]
enum Assembled {
    /// Every chunk of a transaction, concatenated.
    Complete {
        versionstamp: [u8; 10],
        id: [u8; 16],
        blob: Vec<u8>,
    },
    /// A transaction whose chunks could not be reassembled.
    Broken {
        versionstamp: [u8; 10],
        id: [u8; 16],
    },
}

/// A multi-chunk transaction being reassembled.
#[derive(Debug)]
struct Partial {
    /// Key of its first chunk, where to resume while it is pending.
    first_key: Vec<u8>,
    /// Versionstamp of its first chunk.
    versionstamp: [u8; 10],
    /// Versionstamp of its last chunk so far.
    last_versionstamp: [u8; 10],
    total: u32,
    next_chunk: u32,
    /// Concatenated chunk values so far. Grows by the actual chunk lengths only, never by
    /// the untrusted total chunk count.
    blob: Vec<u8>,
    /// Commit version of its first chunk.
    start_version: i64,
    /// Byte offset of the row of its first chunk: bytes fed before it.
    start_offset: u64,
}

/// Reassembles transactions from chunk rows fed in key order. Pure, no I/O.
///
/// A multi-chunk transaction starts at chunk 1 and its chunks come in order 1..=total,
/// with the same id and total and non-decreasing versionstamps: the client may write them
/// in several commits, so rows of other transactions can sit in between. Anything else
/// drops the partial buffer and reports the transaction as [`Assembled::Broken`] (once
/// per id until a new chunk 1 of that id), as does a transaction still pending after a
/// row more than [`max_pending_versions`](ProfileScanner::max_pending_versions) after its
/// first chunk, or after more than
/// [`max_pending_bytes`](ProfileScanner::max_pending_bytes) bytes of rows since its first
/// chunk. Both are measured from each first chunk, so the outcome does not depend on
/// where the scan started, as long as it started at or before that chunk.
///
/// Outcomes produced by a row whose key is below `emitted_up_to` are suppressed: an
/// earlier page, which read that row too, already reported them.
#[derive(Debug)]
struct Assembler {
    /// Key right after the last row read by the previous pages.
    emitted_up_to: Vec<u8>,
    /// Pending transactions by id.
    partials: HashMap<[u8; 16], Partial>,
    /// Ids of the pending transactions by the byte offset of their first chunk, oldest
    /// first. Offsets are unique: every row has a non-empty key.
    by_start: BTreeMap<u64, [u8; 16]>,
    /// Ids already reported (or suppressed) as broken, so that their remaining chunks are
    /// consumed silently. A new chunk 1 of the id removes it.
    broken: HashSet<[u8; 16]>,
    /// Key and value bytes fed so far.
    bytes: u64,
    /// Last row fed.
    last_key: Option<Vec<u8>>,
    /// [`ProfileScanner::max_pending_versions`] of the page being built.
    max_pending_versions: i64,
    /// [`ProfileScanner::max_pending_bytes`] of the page being built.
    max_pending_bytes: u64,
}

impl Assembler {
    fn new(emitted_up_to: Vec<u8>, max_pending_versions: i64, max_pending_bytes: u64) -> Self {
        Assembler {
            emitted_up_to,
            partials: HashMap::new(),
            by_start: BTreeMap::new(),
            broken: HashSet::new(),
            bytes: 0,
            last_key: None,
            max_pending_versions,
            max_pending_bytes,
        }
    }

    /// Feeds one row, pushing to `out` what it completes or breaks.
    fn push(&mut self, key: &[u8], value: &[u8], out: &mut Vec<Assembled>) {
        let offset = self.bytes;
        let len = (key.len() as u64).saturating_add(value.len() as u64);
        self.bytes = self.bytes.saturating_add(len);
        self.last_key = Some(key.to_vec());
        let emit = key >= self.emitted_up_to.as_slice();
        let parsed = ChunkKey::parse(key);
        match parsed {
            Some(parsed) => self.process(offset, parsed, key, value, emit, out),
            None => tracing::warn!(key = ?key, "ignoring unparseable profiling key"),
        }
        let version = parsed.map(|p| version_of(&p.versionstamp));
        self.evict(version, emit, out);
    }

    /// Breaks the transactions still pending after the row just fed, whose version is
    /// `version` (`None` for an unparseable key), when it is more than
    /// `max_pending_versions` after their first chunk or when more than
    /// `max_pending_bytes` were fed since their first chunk.
    ///
    /// Rows are fed in key order, so the oldest pending transaction also has the smallest
    /// first version: checking from the oldest stops at the first one within both bounds.
    fn evict(&mut self, version: Option<i64>, emit: bool, out: &mut Vec<Assembled>) {
        while let Some((&start, &id)) = self.by_start.first_key_value() {
            let pending_bytes = self.bytes.saturating_sub(start);
            let pending_versions = match (version, self.partials.get(&id)) {
                (Some(version), Some(partial)) => version.saturating_sub(partial.start_version),
                _ => 0,
            };
            if pending_bytes <= self.max_pending_bytes
                && pending_versions <= self.max_pending_versions
            {
                break;
            }
            self.by_start.remove(&start);
            if let Some(partial) = self.partials.remove(&id) {
                tracing::debug!(
                    pending_bytes,
                    pending_versions,
                    "evicting incomplete transaction"
                );
                self.report_broken(partial.versionstamp, id, emit, out);
            }
        }
    }

    /// Processes a parsed row, `offset` being the bytes fed before it.
    fn process(
        &mut self,
        offset: u64,
        parsed: ChunkKey,
        key: &[u8],
        value: &[u8],
        emit: bool,
        out: &mut Vec<Assembled>,
    ) {
        if let Some(mut partial) = self.partials.remove(&parsed.id) {
            self.by_start.remove(&partial.start_offset);
            if parsed.total == partial.total
                && parsed.chunk == partial.next_chunk
                && parsed.versionstamp >= partial.last_versionstamp
            {
                partial.blob.extend_from_slice(value);
                if parsed.chunk == partial.total {
                    if emit {
                        out.push(Assembled::Complete {
                            versionstamp: partial.versionstamp,
                            id: parsed.id,
                            blob: partial.blob,
                        });
                    }
                } else {
                    // chunk < total, so this cannot overflow
                    partial.next_chunk += 1;
                    partial.last_versionstamp = parsed.versionstamp;
                    self.by_start.insert(partial.start_offset, parsed.id);
                    self.partials.insert(parsed.id, partial);
                }
                return;
            }
            self.report_broken(partial.versionstamp, parsed.id, emit, out);
        }

        if parsed.chunk == 1 && parsed.total >= 1 {
            // a new transaction under this id, its chunks are no longer those of a broken one
            self.broken.remove(&parsed.id);
        }
        if parsed.chunk == 1 && parsed.total == 1 {
            if emit {
                out.push(Assembled::Complete {
                    versionstamp: parsed.versionstamp,
                    id: parsed.id,
                    blob: value.to_vec(),
                });
            }
        } else if parsed.chunk == 1 && parsed.total > 1 {
            self.by_start.insert(offset, parsed.id);
            self.partials.insert(
                parsed.id,
                Partial {
                    first_key: key.to_vec(),
                    versionstamp: parsed.versionstamp,
                    last_versionstamp: parsed.versionstamp,
                    total: parsed.total,
                    next_chunk: 2,
                    blob: value.to_vec(),
                    start_version: version_of(&parsed.versionstamp),
                    start_offset: offset,
                },
            );
        } else {
            self.report_broken(parsed.versionstamp, parsed.id, emit, out);
        }
    }

    fn report_broken(
        &mut self,
        versionstamp: [u8; 10],
        id: [u8; 16],
        emit: bool,
        out: &mut Vec<Assembled>,
    ) {
        if self.broken.insert(id) && emit {
            out.push(Assembled::Broken { versionstamp, id });
        }
    }

    /// First chunk key of the oldest pending transaction.
    fn pending_first_key(&self) -> Option<&[u8]> {
        let (_, id) = self.by_start.first_key_value()?;
        self.partials.get(id).map(|p| p.first_key.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vs(version: u64, batch: u16) -> [u8; 10] {
        let mut out = [0u8; 10];
        out[..8].copy_from_slice(&version.to_be_bytes());
        out[8..].copy_from_slice(&batch.to_be_bytes());
        out
    }

    fn id(n: u8) -> [u8; 16] {
        [b'0' + n; 16]
    }

    fn key(versionstamp: [u8; 10], id: [u8; 16], chunk: u32, total: u32) -> Vec<u8> {
        let mut key = PROFILE_PREFIX.to_vec();
        key.extend_from_slice(&versionstamp);
        key.push(b'/');
        key.extend_from_slice(&id);
        key.push(b'/');
        key.extend_from_slice(&chunk.to_be_bytes());
        key.extend_from_slice(&total.to_be_bytes());
        key.extend_from_slice(b"/user-id");
        key
    }

    type Rows = Vec<(Vec<u8>, Vec<u8>)>;

    /// Feeds all rows from the beginning with the default reassembly limits, returns the
    /// outputs and the pending first key.
    fn reassemble(rows: &[(Vec<u8>, Vec<u8>)]) -> (Vec<Assembled>, Option<Vec<u8>>) {
        reassemble_with_limits(
            rows,
            DEFAULT_MAX_PENDING_VERSIONS,
            DEFAULT_MAX_PENDING_BYTES,
        )
    }

    /// Like [`reassemble`], with custom reassembly limits.
    fn reassemble_with_limits(
        rows: &[(Vec<u8>, Vec<u8>)],
        max_pending_versions: i64,
        max_pending_bytes: u64,
    ) -> (Vec<Assembled>, Option<Vec<u8>>) {
        let mut assembler = Assembler::new(
            PROFILE_PREFIX.to_vec(),
            max_pending_versions,
            max_pending_bytes,
        );
        let mut out = Vec::new();
        for (k, v) in rows {
            assembler.push(k, v, &mut out);
        }
        let pending = assembler.pending_first_key().map(<[u8]>::to_vec);
        (out, pending)
    }

    fn after(key: &[u8]) -> Vec<u8> {
        [key, b"\x00"].concat()
    }

    fn complete(versionstamp: [u8; 10], id: [u8; 16], blob: &[u8]) -> Assembled {
        Assembled::Complete {
            versionstamp,
            id,
            blob: blob.to_vec(),
        }
    }

    fn broken(versionstamp: [u8; 10], id: [u8; 16]) -> Assembled {
        Assembled::Broken { versionstamp, id }
    }

    #[test]
    fn parses_real_key_layout() {
        let k = key(vs(7, 1), id(1), 2, 3);
        assert_eq!(
            ChunkKey::parse(&k),
            Some(ChunkKey {
                versionstamp: vs(7, 1),
                id: id(1),
                chunk: 2,
                total: 3,
            })
        );
        // the tail after the chunk fields is ignored, and may be absent
        assert!(ChunkKey::parse(&k[..MIN_KEY_LEN]).is_some());
        for len in 0..MIN_KEY_LEN {
            assert_eq!(ChunkKey::parse(&k[..len]), None, "len {len}");
        }
        let mut bad = k.clone();
        bad[VERSIONSTAMP_END] = b'x';
        assert_eq!(ChunkKey::parse(&bad), None);
    }

    #[test]
    fn single_and_multi_chunk() {
        let (out, pending) = reassemble(&[
            (key(vs(1, 0), id(1), 1, 1), b"one".to_vec()),
            (key(vs(1, 0), id(2), 1, 3), b"ab".to_vec()),
            (key(vs(1, 0), id(2), 2, 3), b"cd".to_vec()),
            (key(vs(1, 0), id(2), 3, 3), b"e".to_vec()),
        ]);
        assert_eq!(
            out,
            vec![
                complete(vs(1, 0), id(1), b"one"),
                complete(vs(1, 0), id(2), b"abcde")
            ]
        );
        assert_eq!(pending, None);
    }

    #[test]
    fn straddling_transaction_with_other_rows_in_between() {
        // id 1 has chunks 1-2 in the first commit and chunk 3 in the second one, with
        // rows of other transactions of both commits in between.
        let (out, pending) = reassemble(&[
            (key(vs(1, 0), id(1), 1, 3), b"a".to_vec()),
            (key(vs(1, 0), id(1), 2, 3), b"b".to_vec()),
            (key(vs(1, 0), id(2), 1, 1), b"B".to_vec()),
            (key(vs(2, 0), id(0), 1, 1), b"Z".to_vec()),
            (key(vs(2, 0), id(1), 3, 3), b"c".to_vec()),
            (key(vs(2, 0), id(3), 1, 1), b"D".to_vec()),
        ]);
        assert_eq!(
            out,
            vec![
                complete(vs(1, 0), id(2), b"B"),
                complete(vs(2, 0), id(0), b"Z"),
                complete(vs(1, 0), id(1), b"abc"),
                complete(vs(2, 0), id(3), b"D"),
            ]
        );
        assert_eq!(pending, None);
    }

    #[test]
    fn interleaved_transactions_both_complete() {
        let (out, _) = reassemble(&[
            (key(vs(1, 0), id(1), 1, 2), b"a".to_vec()),
            (key(vs(1, 0), id(2), 1, 2), b"x".to_vec()),
            (key(vs(2, 0), id(1), 2, 2), b"b".to_vec()),
            (key(vs(2, 0), id(2), 2, 2), b"y".to_vec()),
        ]);
        assert_eq!(
            out,
            vec![
                complete(vs(1, 0), id(1), b"ab"),
                complete(vs(1, 0), id(2), b"xy")
            ]
        );
    }

    #[test]
    fn missing_chunk_is_broken_once() {
        let (out, pending) = reassemble(&[
            (key(vs(1, 0), id(1), 1, 4), b"a".to_vec()),
            (key(vs(1, 0), id(1), 2, 4), b"b".to_vec()),
            (key(vs(1, 0), id(1), 4, 4), b"d".to_vec()),
            (key(vs(1, 0), id(2), 1, 1), b"B".to_vec()),
        ]);
        assert_eq!(
            out,
            vec![broken(vs(1, 0), id(1)), complete(vs(1, 0), id(2), b"B")]
        );
        assert_eq!(pending, None);
    }

    #[test]
    fn orphan_chunks_are_broken_once() {
        let (out, _) = reassemble(&[
            (key(vs(1, 0), id(1), 2, 3), b"b".to_vec()),
            (key(vs(1, 0), id(1), 3, 3), b"c".to_vec()),
            (key(vs(1, 0), id(2), 1, 1), b"B".to_vec()),
        ]);
        assert_eq!(
            out,
            vec![broken(vs(1, 0), id(1)), complete(vs(1, 0), id(2), b"B")]
        );
    }

    #[test]
    fn out_of_order_or_inconsistent_chunks_are_broken() {
        let (out, _) = reassemble(&[
            (key(vs(1, 0), id(1), 1, 3), b"a".to_vec()),
            (key(vs(1, 0), id(1), 3, 3), b"c".to_vec()),
            (key(vs(1, 0), id(1), 2, 3), b"b".to_vec()),
        ]);
        assert_eq!(out, vec![broken(vs(1, 0), id(1))]);
        // other total
        let (out, _) = reassemble(&[
            (key(vs(1, 0), id(1), 1, 2), b"a".to_vec()),
            (key(vs(1, 0), id(1), 2, 3), b"b".to_vec()),
        ]);
        assert_eq!(out, vec![broken(vs(1, 0), id(1))]);
        // decreasing versionstamp (only possible with rows fed out of key order)
        let (out, _) = reassemble(&[
            (key(vs(2, 0), id(1), 1, 2), b"a".to_vec()),
            (key(vs(1, 0), id(1), 2, 2), b"b".to_vec()),
        ]);
        assert_eq!(out, vec![broken(vs(2, 0), id(1))]);
    }

    #[test]
    fn chunk_one_restarts_a_pending_transaction() {
        let (out, _) = reassemble(&[
            (key(vs(1, 0), id(1), 1, 2), b"a".to_vec()),
            (key(vs(2, 0), id(1), 1, 2), b"x".to_vec()),
            (key(vs(2, 0), id(1), 2, 2), b"y".to_vec()),
        ]);
        assert_eq!(
            out,
            vec![broken(vs(1, 0), id(1)), complete(vs(2, 0), id(1), b"xy")]
        );
    }

    #[test]
    fn nonsense_chunk_numbers_are_broken() {
        for (chunk, total) in [(0, 0), (0, 1), (1, 0), (2, 1), (u32::MAX, u32::MAX)] {
            let (out, _) = reassemble(&[(key(vs(1, 0), id(1), chunk, total), b"a".to_vec())]);
            assert_eq!(out, vec![broken(vs(1, 0), id(1))], "{chunk}/{total}");
        }
        // a huge total does not allocate for it
        let k = key(vs(1, 0), id(1), 1, u32::MAX);
        let (out, pending) = reassemble(&[(k.clone(), b"a".to_vec())]);
        assert_eq!(out, vec![]);
        assert_eq!(pending, Some(k));
    }

    fn broken_of(out: &[Assembled]) -> Vec<&Assembled> {
        out.iter()
            .filter(|o| matches!(o, Assembled::Broken { .. }))
            .collect()
    }

    #[test]
    fn straddler_with_many_rows_in_between_completes() {
        // two large commits put thousands of rows between chunk 1 and its continuation
        let mut rows: Rows = vec![(key(vs(1, 0), id(1), 1, 2), b"a".to_vec())];
        for batch in 1..=5000 {
            rows.push((key(vs(1, batch), id(2), 1, 1), vec![0; 100]));
        }
        rows.push((key(vs(2, 0), id(1), 2, 2), b"b".to_vec()));
        let (out, pending) = reassemble(&rows);
        assert_eq!(broken_of(&out), Vec::<&Assembled>::new());
        assert_eq!(out.len(), 5001);
        assert_eq!(out.last(), Some(&complete(vs(1, 0), id(1), b"ab")));
        assert_eq!(pending, None);
    }

    #[test]
    fn eviction_by_version_distance() {
        let first = 5;
        let last = first + DEFAULT_MAX_PENDING_VERSIONS as u64;
        let within: Rows = vec![
            (key(vs(first, 0), id(1), 1, 2), b"a".to_vec()),
            (key(vs(last, 0), id(2), 1, 1), Vec::new()),
        ];
        let (out, pending) = reassemble(&within);
        assert_eq!(out, vec![complete(vs(last, 0), id(2), b"")]);
        assert_eq!(pending, Some(within[0].0.clone()));

        // its last chunk right at the bound still completes it
        let mut completing = within.clone();
        completing.push((key(vs(last, 1), id(1), 2, 2), b"b".to_vec()));
        let (out, pending) = reassemble(&completing);
        assert_eq!(out.last(), Some(&complete(vs(first, 0), id(1), b"ab")));
        assert_eq!(pending, None);

        // a row one version further evicts it, its late chunk is then consumed silently
        let mut evicting = within.clone();
        evicting.push((key(vs(last + 1, 0), id(2), 1, 1), Vec::new()));
        evicting.push((key(vs(last + 1, 1), id(1), 2, 2), b"b".to_vec()));
        let (out, pending) = reassemble(&evicting);
        assert_eq!(broken_of(&out), vec![&broken(vs(first, 0), id(1))]);
        assert_eq!(out.len(), 3);
        assert_eq!(pending, None);

        // rows before the first chunk, at earlier versions, do not change the decisions
        for (rows, expected) in [(&completing, false), (&evicting, true)] {
            let mut prefixed: Rows = vec![(key(vs(0, 0), id(3), 1, 1), Vec::new())];
            prefixed.extend(rows.iter().cloned());
            let (out, pending) = reassemble(&prefixed);
            assert_eq!(!broken_of(&out).is_empty(), expected);
            assert_eq!(pending, None);
        }
    }

    #[test]
    fn eviction_by_bytes() {
        let first = key(vs(1, 0), id(1), 1, 2);
        let klen = first.len();
        let max = DEFAULT_MAX_PENDING_BYTES as usize;
        // the first chunk and one filler row add up to exactly DEFAULT_MAX_PENDING_BYTES
        let rows_with = |filler: usize| -> Rows {
            vec![
                (first.clone(), b"a".to_vec()),
                (key(vs(1, 1), id(2), 1, 1), vec![0; filler]),
            ]
        };
        let exact = max - 2 * klen - 1;

        let (out, pending) = reassemble(&rows_with(exact));
        assert_eq!(broken_of(&out), Vec::<&Assembled>::new());
        assert_eq!(pending, Some(first.clone()));

        // its last chunk right then still completes it
        let mut completing = rows_with(exact);
        completing.push((key(vs(1, 2), id(1), 2, 2), b"b".to_vec()));
        let (out, pending) = reassemble(&completing);
        assert_eq!(out.last(), Some(&complete(vs(1, 0), id(1), b"ab")));
        assert_eq!(pending, None);

        // one more byte evicts it
        let mut evicting = rows_with(exact + 1);
        evicting.push((key(vs(1, 2), id(1), 2, 2), b"b".to_vec()));
        let (out, pending) = reassemble(&evicting);
        assert_eq!(broken_of(&out), vec![&broken(vs(1, 0), id(1))]);
        assert_eq!(out.len(), 2);
        assert_eq!(pending, None);

        // bytes read before the first chunk do not change the decisions
        for (rows, expected) in [(&completing, false), (&evicting, true)] {
            let mut prefixed: Rows = vec![(key(vs(0, 0), id(3), 1, 1), vec![0; max])];
            prefixed.extend(rows.iter().cloned());
            let (out, pending) = reassemble(&prefixed);
            assert_eq!(!broken_of(&out).is_empty(), expected);
            assert_eq!(pending, None);
        }
    }

    #[test]
    fn custom_max_pending_versions_changes_eviction() {
        let (first, second) = (5u64, 6u64);
        let rows: Rows = vec![
            (key(vs(first, 0), id(1), 1, 2), b"a".to_vec()),
            (key(vs(second, 0), id(2), 1, 1), Vec::new()),
        ];

        // the default limit leaves id 1 pending: one version is nothing next to
        // DEFAULT_MAX_PENDING_VERSIONS
        let (out, pending) = reassemble(&rows);
        assert_eq!(out, vec![complete(vs(second, 0), id(2), b"")]);
        assert_eq!(pending, Some(rows[0].0.clone()));

        // a custom max_pending_versions of 0 evicts it as soon as a later row lands
        let (out, pending) = reassemble_with_limits(&rows, 0, DEFAULT_MAX_PENDING_BYTES);
        assert_eq!(broken_of(&out), vec![&broken(vs(first, 0), id(1))]);
        assert_eq!(pending, None);
    }

    #[test]
    fn custom_max_pending_bytes_changes_eviction() {
        let first = key(vs(1, 0), id(1), 1, 2);
        let rows: Rows = vec![
            (first.clone(), b"a".to_vec()),
            (key(vs(1, 1), id(2), 1, 1), vec![0; 100]),
        ];

        // the default limit leaves id 1 pending: about a hundred bytes is nothing next to
        // DEFAULT_MAX_PENDING_BYTES (16 MiB)
        let (out, pending) = reassemble(&rows);
        assert_eq!(broken_of(&out), Vec::<&Assembled>::new());
        assert_eq!(pending, Some(first.clone()));

        // a custom max_pending_bytes smaller than the pending row evicts it
        let (out, pending) = reassemble_with_limits(&rows, DEFAULT_MAX_PENDING_VERSIONS, 10);
        assert_eq!(broken_of(&out), vec![&broken(vs(1, 0), id(1))]);
        assert_eq!(pending, None);
    }

    #[test]
    fn reused_id_that_breaks_again_is_reported_again() {
        let (out, pending) = reassemble(&[
            (key(vs(1, 0), id(1), 1, 3), b"a".to_vec()),
            (key(vs(1, 0), id(1), 3, 3), b"c".to_vec()),
            (key(vs(2, 0), id(1), 1, 3), b"a".to_vec()),
            (key(vs(2, 0), id(1), 3, 3), b"c".to_vec()),
            (key(vs(3, 0), id(1), 1, 1), b"x".to_vec()),
            (key(vs(4, 0), id(1), 2, 2), b"y".to_vec()),
        ]);
        assert_eq!(
            out,
            vec![
                broken(vs(1, 0), id(1)),
                broken(vs(2, 0), id(1)),
                complete(vs(3, 0), id(1), b"x"),
                broken(vs(4, 0), id(1)),
            ]
        );
        assert_eq!(pending, None);
    }

    #[test]
    fn unparseable_keys_are_consumed_without_output() {
        let short = [PROFILE_PREFIX.as_slice(), b"short"].concat();
        let (out, pending) = reassemble(&[(short, b"x".to_vec())]);
        assert_eq!(out, vec![]);
        assert_eq!(pending, None);
    }

    /// A valid blob with no event, split in `n` chunks.
    fn blob_chunks(n: usize) -> Vec<Vec<u8>> {
        let blob = ProtocolVersion::V7_4.0.to_le_bytes();
        let size = blob.len().div_ceil(n);
        let mut chunks: Vec<Vec<u8>> = blob.chunks(size).map(<[u8]>::to_vec).collect();
        chunks.resize(n, Vec::new());
        chunks
    }

    /// Rows of a transaction whose chunks are spread over the given versionstamps.
    fn txn(id_n: u8, stamps: &[[u8; 10]]) -> Rows {
        let total = stamps.len() as u32;
        blob_chunks(stamps.len())
            .into_iter()
            .zip(stamps)
            .enumerate()
            .map(|(i, (chunk, stamp))| (key(*stamp, id(id_n), i as u32 + 1, total), chunk))
            .collect()
    }

    /// Runs one page over the sorted `rows`, like `read_page`, in batches of `batch` rows.
    fn run_page(rows: &Rows, cursor: &Cursor, max: usize, batch: usize, budget: bool) -> Page {
        let start: Vec<&(Vec<u8>, Vec<u8>)> = rows
            .iter()
            .filter(|(k, _)| k.as_slice() >= cursor.resume())
            .collect();
        let mut page = PageBuilder::new(
            cursor,
            max,
            DEFAULT_MAX_PENDING_VERSIONS,
            DEFAULT_MAX_PENDING_BYTES,
        );
        if start.is_empty() {
            return page.finish(None);
        }
        let batches: Vec<_> = start.chunks(batch).collect();
        for (i, b) in batches.iter().enumerate() {
            let more = i + 1 < batches.len();
            let exceeded = BudgetExceeded {
                kind: foundationdb::BudgetKind::BytesRead,
                used: 2,
                limit: 1,
            };
            let check = || if budget { Err(exceeded) } else { Ok(()) };
            let rows = b.iter().map(|(k, v)| (k.as_slice(), v.as_slice()));
            if let Some(stopped_by) = page.feed_batch(rows, more, check) {
                return page.finish(stopped_by);
            }
        }
        unreachable!("the last batch finishes the page")
    }

    /// Runs pages until exhausted, returns every reported transaction and skip.
    fn run_pages(
        rows: &Rows,
        max: usize,
        batch: usize,
        budget: bool,
    ) -> (Vec<ProfiledTransaction>, Vec<Skipped>, Cursor) {
        let mut cursor = Cursor::beginning();
        let (mut txs, mut skipped) = (Vec::new(), Vec::new());
        for _ in 0..10_000 {
            let page = run_page(rows, &cursor, max, batch, budget);
            txs.extend(page.transactions);
            skipped.extend(page.skipped);
            let next = Cursor::from_bytes(page.next.as_bytes().to_vec()).unwrap();
            assert_eq!(next, page.next);
            if page.exhausted {
                return (txs, skipped, next);
            }
            assert_ne!(next, cursor, "no progress");
            cursor = next;
        }
        panic!("pages never exhausted");
    }

    #[test]
    fn straddling_across_page_boundaries_is_returned_once() {
        let (v1, v2, v3) = (vs(1, 0), vs(2, 0), vs(3, 0));
        let mut rows: Rows = Vec::new();
        rows.extend(txn(1, &[v1, v1, v2]));
        rows.extend(txn(2, &[v1]));
        rows.extend(txn(3, &[v1, v2]));
        rows.extend(txn(4, &[v2]));
        rows.extend(txn(5, &[v2, v2]));
        rows.extend(txn(6, &[v2, v3, v3]));
        rows.extend(txn(7, &[v3]));
        rows.sort();

        let (all, skipped, _) = run_pages(&rows, usize::MAX, rows.len(), false);
        assert!(skipped.is_empty());
        let expected: Vec<_> = all.iter().map(|t| (t.versionstamp, t.id)).collect();
        assert_eq!(expected.len(), 7);
        let mut sorted = expected.clone();
        sorted.sort();

        for batch in 1..=4 {
            for (max, budget) in [(1, false), (2, false), (3, false), (usize::MAX, true)] {
                let (txs, skipped, _) = run_pages(&rows, max, batch, budget);
                let mut got: Vec<_> = txs.iter().map(|t| (t.versionstamp, t.id)).collect();
                got.sort();
                assert_eq!(got, sorted, "batch {batch} max {max} budget {budget}");
                assert!(
                    skipped.is_empty(),
                    "batch {batch} max {max} budget {budget}"
                );
            }
        }
    }

    #[test]
    fn completed_chunks_after_resume_are_not_reemitted() {
        // id 1 is pending at the end of the first page (its last chunk is not written
        // yet), id 2 completes after its first chunk.
        let (v1, v2) = (vs(1, 0), vs(2, 0));
        let mut rows: Rows = Vec::new();
        rows.extend(txn(1, &[v1, v2]));
        rows.extend(txn(2, &[v1, v1]));
        rows.sort();
        let last = rows
            .iter()
            .position(|(k, _)| k[VERSIONSTAMP_START..] >= v2[..]);
        let first_page_rows: Rows = rows[..last.unwrap()].to_vec();

        let page1 = run_page(&first_page_rows, &Cursor::beginning(), 100, 10, false);
        assert!(page1.exhausted);
        assert_eq!(page1.transactions.len(), 1);
        assert_eq!(page1.transactions[0].id, id(2));
        assert_eq!(page1.next.resume(), rows[0].0.as_slice());

        // the second commit lands: page 2 rereads id 2 but only reports id 1
        let page2 = run_page(&rows, &page1.next, 100, 10, false);
        assert!(page2.exhausted);
        assert!(page2.skipped.is_empty());
        let ids: Vec<_> = page2.transactions.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![id(1)]);
        assert_eq!(page2.transactions[0].versionstamp, v1);
    }

    #[test]
    fn orphan_after_resume_is_suppressed() {
        let (v1, v2) = (vs(1, 0), vs(2, 0));
        let mut rows: Rows = Vec::new();
        rows.extend(txn(1, &[v1, v2]));
        // chunk 2 of id 2 without its chunk 1
        rows.push((key(v1, id(2), 2, 2), Vec::new()));
        rows.sort();
        let first_page_rows: Rows = rows[..2].to_vec();

        let page1 = run_page(&first_page_rows, &Cursor::beginning(), 100, 10, false);
        assert_eq!(page1.skipped.len(), 1);
        assert_eq!(page1.skipped[0].id, id(2));
        assert_eq!(page1.skipped[0].reason, SkipReason::BrokenChunks);

        let page2 = run_page(&rows, &page1.next, 100, 10, false);
        assert!(page2.skipped.is_empty(), "{:?}", page2.skipped);
        assert_eq!(page2.transactions.len(), 1);
        assert_eq!(page2.transactions[0].id, id(1));
    }

    #[test]
    fn pending_at_end_of_bounded_read_keeps_resume_at_first_chunk() {
        let (v1, v2) = (vs(1, 0), vs(2, 0));
        let mut rows: Rows = txn(1, &[v1, v2]);
        rows.extend(txn(2, &[v1]));
        rows.sort();
        // what a read bounded by end_version = 2 sees
        let bounded: Rows = rows
            .iter()
            .filter(|(k, _)| k[VERSIONSTAMP_START..] < v2[..])
            .cloned()
            .collect();
        let page = run_page(&bounded, &Cursor::beginning(), 100, 10, false);
        assert!(page.exhausted);
        assert_eq!(page.transactions.len(), 1);
        assert_eq!(page.next.resume(), rows[0].0.as_slice());
        assert_eq!(page.next.emitted_up_to(), after(&bounded[1].0).as_slice());
    }

    #[test]
    fn page_builder_counts_and_decodes() {
        let valid = ProtocolVersion::V7_4.0.to_le_bytes().to_vec();
        let mut page = PageBuilder::new(
            &Cursor::beginning(),
            2,
            DEFAULT_MAX_PENDING_VERSIONS,
            DEFAULT_MAX_PENDING_BYTES,
        );
        assert!(!page.push(&key(vs(5, 0), id(1), 1, 1), &valid));
        // broken chunks do not count towards max_transactions
        assert!(!page.push(&key(vs(5, 0), id(2), 2, 2), b""));
        // a decode failure counts
        assert!(page.push(&key(vs(5, 0), id(3), 1, 1), b"garbage"));
        let last = key(vs(5, 0), id(3), 1, 1);
        let page = page.finish(Some(StopReason::MaxTransactions));
        assert_eq!(
            page.transactions,
            vec![ProfiledTransaction {
                version: 5,
                versionstamp: vs(5, 0),
                id: id(1),
                protocol_version: ProtocolVersion::V7_4,
                events: vec![],
            }]
        );
        assert_eq!(page.skipped.len(), 2);
        assert_eq!(page.skipped[0].reason, SkipReason::BrokenChunks);
        assert!(matches!(page.skipped[1].reason, SkipReason::Decode(_)));
        assert_eq!(page.next.resume(), after(&last).as_slice());
        assert_eq!(page.next.emitted_up_to(), after(&last).as_slice());
        assert!(!page.exhausted);
    }

    #[test]
    fn cursor_round_trip_and_validation() {
        let a = key(vs(1, 0), id(1), 1, 2);
        let b = after(&key(vs(9, 0), id(1), 1, 1));
        for cursor in [
            Cursor::beginning(),
            Cursor::at_version(0),
            Cursor::at_version(123_456),
            Cursor::new(&a, &b),
        ] {
            assert_eq!(
                Cursor::from_bytes(cursor.as_bytes().to_vec()),
                Ok(cursor.clone())
            );
        }
        let c = Cursor::new(&a, &b);
        assert_eq!(
            (c.resume(), c.emitted_up_to()),
            (a.as_slice(), b.as_slice())
        );
        assert_eq!(Cursor::at_version(-5), Cursor::at_version(0));
        let v7 = [PROFILE_PREFIX.as_slice(), &7i64.to_be_bytes(), b"\x00\x00"].concat();
        let c7 = Cursor::at_version(7);
        assert_eq!(
            (c7.resume(), c7.emitted_up_to()),
            (v7.as_slice(), v7.as_slice())
        );

        let valid = c.as_bytes().to_vec();
        let mut garbage: Vec<Vec<u8>> = vec![
            Vec::new(),
            b"nope".to_vec(),
            PROFILE_PREFIX.to_vec(),
            // resume after emitted_up_to
            Cursor::new(&b, &a).as_bytes().to_vec(),
            // keys outside the keyspace
            Cursor::new(b"a", b"b").as_bytes().to_vec(),
            Cursor::new(&a, b"\xff\xff").as_bytes().to_vec(),
        ];
        // other format
        let mut other = valid.clone();
        other[0] = 2;
        garbage.push(other);
        // resume length past the end
        let mut long = valid.clone();
        long[1..5].copy_from_slice(&u32::MAX.to_be_bytes());
        garbage.push(long);
        // every truncation
        for len in 0..valid.len() {
            let truncated = valid[..len].to_vec();
            if truncated.len() > CURSOR_HEADER_LEN + a.len() {
                // truncating emitted_up_to can leave a valid, shorter key
                continue;
            }
            garbage.push(truncated);
        }
        for bytes in garbage {
            assert_eq!(
                Cursor::from_bytes(bytes.clone()),
                Err(InvalidCursor),
                "{bytes:?}"
            );
        }
    }
}
