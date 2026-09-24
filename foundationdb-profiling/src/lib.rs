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
//! retry loop: [`read_page`] takes a [`Transaction`], reads one bounded page, reassembles
//! and decodes the chunks into typed [`Event`]s, and returns a resumable [`Cursor`].
//! Only records written by 7.1+ clients are decoded (see [`decode_events`]).
//!
//! # Example
//!
//! ```no_run
//! use foundationdb::options::TransactionOption;
//! use foundationdb::{ClientBudget, Database, FdbBindingError};
//! use foundationdb_profiling::{Cursor, Page, PageRequest, read_page};
//! use std::time::Duration;
//!
//! # async fn example(db: &Database) -> Result<(), FdbBindingError> {
//! let req = PageRequest {
//!     cursor: Cursor::beginning(),
//!     end_version: None,
//!     max_transactions: 1_000,
//! };
//! let page: Page = db
//!     .run(|trx, _| {
//!         let req = req.clone();
//!         async move {
//!             // Options are the caller's job: this crate never sets any.
//!             trx.set_option(TransactionOption::ReadSystemKeys)?;
//!             trx.set_client_budget(ClientBudget {
//!                 time_limit: Some(Duration::from_secs(2)),
//!                 ..ClientBudget::default()
//!             });
//!             Ok::<_, FdbBindingError>(read_page(&trx, &req).await?)
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
//! # What the caller must set
//!
//! This crate never sets transaction options. On the transaction given to [`read_page`]:
//!
//! - `TransactionOption::ReadSystemKeys` is required, the data lives in the system
//!   keyspace.
//! - `TransactionOption::ReadLockAware` is required if the cluster may be locked (for
//!   instance a DR secondary).
//! - A [`ClientBudget`] bounds a page: [`read_page`] checks it after every batch and
//!   stops with [`StopReason::Budget`] when it is exceeded. Use a `time_limit` well under
//!   the 5 seconds transaction lifetime, or `max_bytes_read`. Without a budget, the page
//!   is only bounded by [`PageRequest::max_transactions`] and the end of the range, and
//!   a large range can hit `transaction_too_old`.
//!
//! # Paging and tailing
//!
//! [`Page::next`] is always a valid place to resume from. To read a range in several
//! transactions, loop on [`read_page`] with `cursor = page.next` until [`Page::exhausted`]:
//! every transaction is returned once over the sequence of pages, even when the client
//! wrote its chunks in several commits and they straddle a page boundary. To tail the
//! keyspace, persist `page.next` ([`Cursor::as_bytes`] / [`Cursor::from_bytes`]) and keep
//! polling from it: an exhausted page's cursor picks up the records flushed after it.
//! [`Cursor::at_version`] starts at a commit version, and [`PageRequest::end_version`]
//! bounds a page by one. A transaction whose chunks straddle `end_version` is not
//! returned by that bounded read, the cursor stays on its first chunk.
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
    Cursor, InvalidCursor, PROFILE_PREFIX, Page, PageRequest, ProfiledTransaction, SkipReason,
    Skipped, StopReason, read_page,
};
