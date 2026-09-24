//! Decodes every transaction currently in the client profiling keyspace, as a check that
//! the decoder handles real client-written data, not just the fixtures and the small
//! scenarios `read_page.rs` writes itself.
//!
//! Unlike `read_page.rs`, this test does not enable or restore profiling itself: it
//! expects to run against a cluster that already has client profiling enabled cluster-wide
//! and has served real traffic. This is exactly what `.github/workflows/profiling.yml`
//! does: it runs `fdbcli --exec "profile client set 1.0 default"`, runs the `foundationdb`
//! crate's test suite as traffic, waits for the client's flush interval, then runs this
//! test. It is `#[ignore]`d so that a plain `cargo test -p foundationdb-profiling` (without
//! profiling enabled) does not fail.

use foundationdb::options::TransactionOption;
use foundationdb::{ClientBudget, Database, FdbBindingError};
use foundationdb_profiling::{Aggregator, Cursor, Event, Page, PageRequest, SkipReason, read_page};
use std::collections::BTreeMap;
use std::time::Duration;

/// Upper bound on the number of pages read, so a bug that never exhausts the range fails
/// fast instead of hanging.
const MAX_PAGES: usize = 100_000;

#[tokio::test]
#[ignore = "needs a live cluster with client profiling enabled and real traffic, see profiling.yml"]
async fn decode_all_client_latency_transactions() {
    let db = Database::default().expect("database");

    let mut cursor = Cursor::beginning();
    let mut aggregator = Aggregator::default();
    let mut total_transactions = 0usize;
    let mut event_counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut decode_errors = Vec::new();
    let mut broken_chunks = 0usize;

    for _ in 0..MAX_PAGES {
        let page = read_one_page(&db, &cursor).await;

        for tx in &page.transactions {
            total_transactions += 1;
            for event in &tx.events {
                *event_counts.entry(event_name(event)).or_insert(0) += 1;
            }
            aggregator.record(tx);
        }
        for skipped in &page.skipped {
            match &skipped.reason {
                SkipReason::Decode(err) => {
                    decode_errors.push((skipped.versionstamp, skipped.id, err.clone()));
                }
                SkipReason::BrokenChunks => {
                    broken_chunks += 1;
                    println!(
                        "broken chunks: versionstamp={:?} id={:?}",
                        skipped.versionstamp, skipped.id
                    );
                }
            }
        }

        cursor = page.next;
        if page.exhausted {
            break;
        }
    }

    println!("transactions decoded: {total_transactions}");
    println!("broken chunks skipped: {broken_chunks}");
    println!("event counts:");
    for (name, count) in &event_counts {
        println!("  {name}: {count}");
    }
    println!("reads total: {}", aggregator.reads().total());
    println!("writes total: {}", aggregator.writes().total());

    if !decode_errors.is_empty() {
        for (versionstamp, id, err) in &decode_errors {
            println!("decode error: versionstamp={versionstamp:?} id={id:?} error={err}");
        }
        panic!(
            "{} transaction(s) failed to decode, see printed ids/errors above",
            decode_errors.len()
        );
    }
    assert!(
        total_transactions >= 1,
        "no transactions were read; is client profiling enabled and has the cluster served traffic?"
    );
}

/// Reads one page with a small time-bounded budget, like the crate docs example.
async fn read_one_page(db: &Database, cursor: &Cursor) -> Page {
    let req = PageRequest {
        cursor: cursor.clone(),
        end_version: None,
        max_transactions: 100_000,
    };
    db.run(|trx, _| {
        let req = req.clone();
        async move {
            trx.set_option(TransactionOption::ReadSystemKeys)?;
            trx.set_client_budget(ClientBudget {
                time_limit: Some(Duration::from_secs(2)),
                ..ClientBudget::default()
            });
            Ok::<_, FdbBindingError>(read_page(&trx, &req).await?)
        }
    })
    .await
    .expect("read_page")
}

fn event_name(event: &Event) -> &'static str {
    match event {
        Event::GetVersion(_) => "GetVersion",
        Event::Get(_) => "Get",
        Event::GetRange(_) => "GetRange",
        Event::Commit(_) => "Commit",
        Event::GetError(_) => "GetError",
        Event::GetRangeError(_) => "GetRangeError",
        Event::CommitError(_) => "CommitError",
    }
}
