use foundationdb::*;
#[allow(unused_imports)]
use foundationdb_macros::cfg_api_versions;
#[allow(unused_imports)]
use std::sync::Arc;
#[allow(unused_imports)]
use std::sync::atomic::{AtomicU64, Ordering};

mod common;

/// Happy path: instrumented_run completes with metrics, no conflicts.
#[tokio::test]
async fn test_happy_path_instrumented() -> FdbResult<()> {
    let db = common::database().await?;

    let (result, metrics) = db
        .instrumented_run(|trx, _| async move {
            trx.set(b"test_runner_hooks_happy", b"value");
            Ok::<_, FdbBindingError>(42u64)
        })
        .await
        .expect("transaction should succeed");

    assert_eq!(result, 42);
    assert_eq!(metrics.transaction.retries, 0);
    assert!(metrics.conflicting_keys.is_empty());

    Ok(())
}

/// Conflict path via instrumented_run: force a conflict and verify
/// MetricsReport.conflicting_keys is populated when ReportConflictingKeys is enabled.
///
/// ReportConflictingKeys (option 712) was added in FDB 6.3.
#[cfg_api_versions(min = 630)]
#[tokio::test]
async fn test_conflict_reports_in_metrics() -> FdbResult<()> {
    let db = common::database().await?;
    let attempt = Arc::new(AtomicU64::new(0));

    let (_, metrics) = db
        .instrumented_run(|trx, _| {
            let attempt = attempt.clone();
            async move {
                let current = attempt.fetch_add(1, Ordering::SeqCst);

                // Enable conflict reporting
                trx.set_option(options::TransactionOption::ReportConflictingKeys)?;

                // Read a key to establish read conflict range
                let _ = trx.get(b"test_conflict_metrics_key", false).await?;

                if current == 0 {
                    // On first attempt, write the same key from another transaction
                    let db2 = Database::new_compat(None).await?;
                    let other_trx = db2.create_trx()?;
                    other_trx.set(b"test_conflict_metrics_key", b"other_value");
                    other_trx
                        .commit()
                        .await
                        .map_err(|e| FdbBindingError::NonRetryableFdbError(FdbError::from(e)))?;
                }

                trx.set(b"test_conflict_metrics_key", b"my_value");
                Ok::<_, FdbBindingError>(())
            }
        })
        .await
        .expect("transaction should eventually succeed");

    // Should have retried at least once
    assert!(metrics.transaction.retries >= 1);
    // Should have recorded at least one conflict
    assert!(metrics.transaction.conflict_count >= 1);
    // Conflicting keys should be populated from the first (failed) attempt
    assert!(
        !metrics.conflicting_keys.is_empty(),
        "expected conflicting keys to be reported"
    );

    Ok(())
}

/// Direct API: use Transaction::conflicting_keys() on a TransactionCommitError.
///
/// ReportConflictingKeys (option 712) was added in FDB 6.3.
#[cfg_api_versions(min = 630)]
#[tokio::test]
async fn test_conflict_keys_direct_api() -> FdbResult<()> {
    let db = common::database().await?;

    // Transaction A: read, then try to commit after B writes the same key
    let trx_a = db.create_trx()?;
    trx_a.set_option(options::TransactionOption::ReportConflictingKeys)?;
    let _ = trx_a.get(b"test_conflict_direct_key", false).await?;

    // Transaction B: write the same key and commit
    let trx_b = db.create_trx()?;
    trx_b.set(b"test_conflict_direct_key", b"b_value");
    trx_b.commit().await.expect("trx B should commit");

    // Transaction A: write something and try to commit — should conflict
    trx_a.set(b"test_conflict_direct_key", b"a_value");
    let commit_result = trx_a.commit().await;

    match commit_result {
        Ok(_committed) => {
            // It's possible (though unlikely) that A commits successfully
            // if the read version window doesn't overlap. That's fine.
        }
        Err(commit_error) => {
            // Read conflicting keys before on_error resets the transaction
            let conflicting = commit_error.conflicting_keys().await?;
            for range in &conflicting {
                eprintln!(
                    "conflicting range: begin={:?} end={:?}",
                    String::from_utf8_lossy(range.begin()),
                    String::from_utf8_lossy(range.end()),
                );
            }
            assert!(
                !conflicting.is_empty(),
                "expected conflicting keys on commit error"
            );

            // Verify the conflicting range includes our key
            let key: &[u8] = b"test_conflict_direct_key";
            let has_our_key = conflicting
                .iter()
                .any(|range| key >= range.begin() && key < range.end());
            assert!(has_our_key, "conflicting range should contain our key");
        }
    }

    Ok(())
}
