//! Read FoundationDB client transaction profiling data.
//!
//! When client profiling is enabled (`fdbcli> profile client set <rate> <size limit>`),
//! every FoundationDB client samples transactions at the given rate and records their
//! operations (read version, gets, range reads, commits and their errors). Every
//! `CSI_STATUS_DELAY` (10 seconds by default) the client flushes the recorded
//! transactions into the system keyspace under [`PROFILE_PREFIX`]
//! (`\xff\x02/fdbClientInfo/client_latency/`), split in chunks keyed by versionstamp and
//! transaction id. See the [transaction profiler documentation].
//!
//! This crate reads that keyspace from an application that owns its [`Database`] and
//! retry loop: build a [`ProfileScanner`] once and call [`ProfileScanner::read_page`]
//! with a [`Transaction`] to read one bounded page, reassemble and decode the chunks
//! into typed [`Event`]s, and return a resumable [`Cursor`]. Only records written by
//! 7.1+ clients are decoded (see [`decode_events`]).
//!
//! # Example
//!
//! ```no_run
//! use foundationdb::options::TransactionOption;
//! use foundationdb::{Database, FdbBindingError};
//! use foundationdb_profiling::{Cursor, Page, ProfileScanner};
//!
//! # async fn example(db: &Database) -> Result<(), FdbBindingError> {
//! let scanner = ProfileScanner::new();
//! let cursor = Cursor::beginning();
//! let page: Page = db
//!     .run(|trx, _| {
//!         let scanner = scanner.clone();
//!         let cursor = cursor.clone();
//!         async move {
//!             // Options are the caller's job: this crate never sets any.
//!             trx.set_option(TransactionOption::ReadSystemKeys)?;
//!             Ok::<_, FdbBindingError>(scanner.read_page(&trx, &cursor).await?)
//!         }
//!     })
//!     .await?;
//! for tx in &page.transactions {
//!     println!("{} {:?}: {} events", tx.version, tx.id, tx.events.len());
//! }
//! // Persist `page.next.as_bytes()` to resume from there later.
//! # Ok(())
//! # }
//! ```
//!
//! See `examples/top_keys.rs` for a runnable end-to-end example that pages through the
//! whole keyspace and prints the hottest keys, ranges and write hot spots.
//!
//! # What the caller must set
//!
//! This crate never sets transaction options. On the transaction given to
//! [`ProfileScanner::read_page`]:
//!
//! - `TransactionOption::ReadSystemKeys` is required, the data lives in the system
//!   keyspace.
//! - `TransactionOption::ReadLockAware` is required if the cluster may be locked (for
//!   instance a DR secondary).
//!
//! [`ProfileScanner::read_page`] sets its own [`ClientBudget`] on the transaction
//! (replacing any budget the caller set directly) and stops with [`StopReason::Budget`]
//! when it is exceeded, checked after every batch. Configure it with
//! [`ProfileScanner::budget`]; the default uses a `time_limit` well under the 5 second
//! transaction lifetime. Without a time or byte limit, a page is only bounded by
//! [`ProfileScanner::max_transactions`] and the end of the range, and a large range can
//! hit `transaction_too_old`.
//!
//! # Paging and tailing
//!
//! [`Page::next`] is always a valid place to resume from. To read a range in several
//! transactions, loop on [`ProfileScanner::read_page`] with `cursor = page.next` until
//! [`Page::exhausted`]: every transaction is returned once over the sequence of pages,
//! even when the client wrote its chunks in several commits and they straddle a page
//! boundary. To tail the keyspace, persist `page.next` ([`Cursor::as_bytes`] /
//! [`Cursor::from_bytes`]) and keep polling from it: an exhausted page's cursor picks up
//! the records flushed after it. [`Cursor::at_version`] starts at a commit version, and
//! [`ProfileScanner::end_version`] bounds a page by one. A transaction whose chunks
//! straddle `end_version` is not returned by that bounded read, the cursor stays on its
//! first chunk.
//!
//! Build one [`ProfileScanner`] and reuse it for every page of a scan: its reassembly
//! limits ([`ProfileScanner::max_pending_versions`], [`ProfileScanner::max_pending_bytes`])
//! must stay the same across the pages that share a cursor, or a transaction can be
//! returned twice or lost.
//!
//! Note that the version of a record is the one at which the client flushed it, some time
//! after the profiled transaction ran.
//!
//! # When profiling data is flushed
//!
//! The C++ client records a sampled transaction's events when its native transaction is
//! destroyed. With these Rust bindings, that happens when the [`Transaction`] (or the
//! `RetryableTransaction` of a [`Database::run`] closure) is dropped. In particular a
//! `TransactionCommitError` owns the transaction and keeps it alive until the error is
//! dropped (or recovered with `on_error`), so holding on to such errors delays the
//! profiling data of the failed transaction. The data then reaches the keyspace at the
//! client's next flush, up to `CSI_STATUS_DELAY` later.
//!
//! # Aggregation
//!
//! [`Aggregator`] counts the keys and ranges read and written by the transactions of one
//! or more pages, like the Python `transaction_profiling_analyzer`.
//!
//! [transaction profiler documentation]: https://apple.github.io/foundationdb/transaction-profiler-analyzer.html
//! [`Database`]: foundationdb::Database
//! [`Database::run`]: foundationdb::Database::run
//! [`Transaction`]: foundationdb::Transaction
//! [`ClientBudget`]: foundationdb::ClientBudget

mod aggregate;
mod decode;
mod event;
mod reader;

pub use aggregate::{Aggregator, Bucket, KeyCounts};
pub use decode::{DecodeError, decode_events};
pub use event::{
    Commit, CommitError, CommitRequest, Event, EventHeader, Get, GetError, GetRange, GetRangeError,
    GetVersion, KeyRange, Mutation, ProtocolVersion, SpanContext,
};
pub use reader::{
    Cursor, InvalidCursor, PROFILE_PREFIX, Page, ProfileScanner, ProfiledTransaction, SkipReason,
    Skipped, StopReason,
};
