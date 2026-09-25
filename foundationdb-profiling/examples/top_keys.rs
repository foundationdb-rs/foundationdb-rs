//! Reads the whole client profiling keyspace and prints the hottest keys, ranges and
//! write hot spots, the way an application embedding this crate would.
//!
//! Enable client profiling on the cluster first, for instance for 1% of transactions:
//!
//! ```text
//! fdbcli --exec "profile client set 0.01 default"
//! ```
//!
//! Then, once the client has served some traffic and flushed its samples, run this
//! example against the same cluster (optionally pass a cluster file as the first
//! argument, otherwise the default one is used):
//!
//! ```text
//! cargo run -p foundationdb-profiling --example top_keys --features embedded-fdb-include
//! ```

use foundationdb::options::TransactionOption;
use foundationdb::tuple::Bytes;
use foundationdb::{ClientBudget, Database, FdbBindingError};
use foundationdb_profiling::{Aggregator, Cursor, PageRequest, SkipReason, read_page};
use std::time::Duration;

const TOP_N: usize = 10;
const BUCKET_COUNT: usize = 10;

#[tokio::main]
async fn main() {
    foundationdb::boot().expect("failed to initialize FoundationDB");

    let cluster_file = std::env::args().nth(1);
    let db = Database::new(cluster_file.as_deref()).expect("failed to open database");

    let mut cursor = Cursor::beginning();
    let mut aggregator = Aggregator::default();
    let mut read = 0usize;
    let (mut decode_errors, mut broken_chunks) = (0usize, 0usize);

    loop {
        let req = PageRequest {
            cursor: cursor.clone(),
            end_version: None,
            max_transactions: 1_000,
        };
        let page = db
            .run(|trx, _maybe_committed| {
                let req = req.clone();
                async move {
                    // read_page never sets transaction options or a budget itself: that
                    // is the caller's job, see the crate docs.
                    trx.set_option(TransactionOption::ReadSystemKeys)?;
                    trx.set_client_budget(ClientBudget {
                        time_limit: Some(Duration::from_secs(2)),
                        ..ClientBudget::default()
                    });
                    Ok::<_, FdbBindingError>(read_page(&trx, &req).await?)
                }
            })
            .await
            .expect("failed to read a page of profiling data");

        read += page.transactions.len();
        for tx in &page.transactions {
            aggregator.record(tx);
        }
        for skipped in &page.skipped {
            match skipped.reason {
                SkipReason::Decode(_) => decode_errors += 1,
                SkipReason::BrokenChunks => broken_chunks += 1,
            }
        }

        cursor = page.next;
        if page.exhausted {
            break;
        }
    }

    println!("transactions read: {read}");
    println!(
        "transactions skipped: {} ({decode_errors} decode error(s), {broken_chunks} broken chunk(s))",
        decode_errors + broken_chunks
    );

    println!("\ntop {TOP_N} read keys:");
    for (key, count) in aggregator.reads().top_keys(TOP_N) {
        println!("  {count:>8}  {}", Bytes::from(key));
    }

    println!("\ntop {TOP_N} read ranges:");
    for (range, count) in aggregator.reads().top_ranges(TOP_N) {
        println!(
            "  {count:>8}  {} .. {}",
            Bytes::from(range.begin),
            Bytes::from(range.end)
        );
    }

    println!("\ntop {TOP_N} written keys:");
    for (key, count) in aggregator.writes().top_keys(TOP_N) {
        println!("  {count:>8}  {}", Bytes::from(key));
    }

    println!("\n{BUCKET_COUNT} read buckets:");
    for bucket in aggregator.reads().buckets(BUCKET_COUNT) {
        println!("  {:>8}  {}", bucket.count, Bytes::from(bucket.start));
    }
}
