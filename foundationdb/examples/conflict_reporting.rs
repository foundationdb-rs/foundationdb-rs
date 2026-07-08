//! This example demonstrates how to use custom [`RunnerHooks`] to observe
//! the transaction retry loop lifecycle, including conflict reporting.
//!
//! It creates two transactions that conflict on the same key, showing
//! the full hook lifecycle: commit error → conflicting keys → retry → success.

use foundationdb::options::TransactionOption;
use foundationdb::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// A simple hook implementation that prints each lifecycle event.
struct PrintHooks;

impl RunnerHooks for PrintHooks {
    async fn on_commit_error(&self, err: &TransactionCommitError) -> FdbResult<()> {
        let keys = err.conflicting_keys().await?;
        println!("  on_commit_error: {} conflicting range(s)", keys.len());
        for range in &keys {
            println!(
                "    {:?} .. {:?}",
                String::from_utf8_lossy(range.begin()),
                String::from_utf8_lossy(range.end()),
            );
        }
        Ok(())
    }

    fn on_closure_error(&self, err: &FdbError) {
        println!("  on_closure_error: {}", err.message());
    }

    fn on_error_duration(&self, ms: u64) {
        println!("  on_error_duration: {ms}ms");
    }

    fn on_commit_success(&self, _committed: &TransactionCommitted, ms: u64) {
        println!("  on_commit_success: committed in {ms}ms");
    }

    fn on_retry(&self) {
        println!("  on_retry");
    }

    fn on_complete(&self) {
        println!("  on_complete");
    }
}

#[tokio::main]
async fn main() {
    foundationdb::boot().expect("failed to initialize FoundationDB");

    if let Err(e) = run_example().await {
        eprintln!("Error: {e:?}");
    }
}

async fn run_example() -> Result<(), FdbBindingError> {
    let db = Database::default()?;
    let attempt = Arc::new(AtomicU64::new(0));

    println!("Running transaction with PrintHooks (forcing a conflict)...");

    let hooks = PrintHooks;
    db.run_with_hooks(&hooks, |trx, _| {
        let attempt = attempt.clone();
        async move {
            let current = attempt.fetch_add(1, Ordering::SeqCst);

            // Enable conflict reporting
            trx.set_option(TransactionOption::ReportConflictingKeys)?;

            // Read a key to establish a read conflict range
            let _ = trx.get(b"example_conflict_key", false).await?;

            if current == 0 {
                // On first attempt, have another transaction write to the same key
                let db2 = Database::default()?;
                let other = db2.create_trx()?;
                other.set(b"example_conflict_key", b"sneaky_write");
                other
                    .commit()
                    .await
                    .map_err(|e| FdbBindingError::NonRetryableFdbError(FdbError::from(e)))?;
                println!("  (injected conflicting write)");
            }

            trx.set(b"example_conflict_key", b"my_value");
            Ok(())
        }
    })
    .await?;

    println!("Transaction succeeded after conflict!");
    Ok(())
}

/*
// Expected output:
//
// Running transaction with PrintHooks (forcing a conflict)...
//   (injected conflicting write)
//   on_commit_error: 1 conflicting range(s)
//     "example_conflict_key" .. "example_conflict_key\0"
//   on_error_duration: 0ms
//   on_retry
//   on_commit_success: committed in 1ms
// Transaction succeeded after conflict!
*/
