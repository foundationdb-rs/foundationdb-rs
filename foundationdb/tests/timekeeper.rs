use foundationdb::timekeeper::{HintMode, hint_version_from_timestamp};
use std::time::SystemTime;

/// Upper bound, in seconds, on how long to wait for the cluster to write a timekeeper entry.
const TIMEKEEPER_POLL_ATTEMPTS: u64 = 60;

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("Unable to get timestamp")
        .as_secs()
}

#[tokio::test]
async fn timekeeper() {
    let database = foundationdb::Database::new_compat(None)
        .await
        .expect("Unable to create database");
    let now = unix_now();
    // The cluster controller writes a timekeeper entry roughly every 10 seconds, but the
    // exact cadence is not guaranteed (slow CI runners, freshly created clusters), so a
    // single fixed sleep is racy. Poll with a fresh transaction each time (the read
    // version must be newer than the entry) until an entry at or after `now` shows up.
    let mut found = None;
    for _ in 0..TIMEKEEPER_POLL_ATTEMPTS {
        let trx = database.create_trx().expect("Unable to create transaction");
        found = hint_version_from_timestamp(&trx, now, HintMode::AfterTimestamp, true)
            .await
            .expect("Unable to get hint version");
        if found.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    assert!(
        found.is_some(),
        "no timekeeper entry at or after {now} within {TIMEKEEPER_POLL_ATTEMPTS}s"
    );

    // A timestamp far in the future has no entry yet. Computed after polling so that
    // it stays ahead of every entry the cluster may have written in the meantime.
    let future_date = unix_now() + 3600;
    let trx = database.create_trx().expect("Unable to create transaction");
    let result = hint_version_from_timestamp(&trx, future_date, HintMode::AfterTimestamp, true)
        .await
        .expect("Unable to get hint version");
    assert!(result.is_none());

    // create a new transaction to get the first read version greater than a long past timestamp
    let trx = database.create_trx().expect("Unable to create transaction");
    let past_date = 0;
    let result = hint_version_from_timestamp(&trx, past_date, HintMode::AfterTimestamp, true)
        .await
        .expect("Unable to get hint version");
    assert!(result.is_some());

    // create a new transaction to get the first available read version
    let trx = database.create_trx().expect("Unable to create transaction");
    let result = hint_version_from_timestamp(&trx, future_date, HintMode::BeforeTimestamp, true)
        .await
        .expect("Unable to get hint version");
    assert!(result.is_some());

    // create a new transaction to fail getting older read version
    let trx = database.create_trx().expect("Unable to create transaction");
    let past_date = 0;
    let result = hint_version_from_timestamp(&trx, past_date, HintMode::BeforeTimestamp, true)
        .await
        .expect("Unable to get hint version");
    assert!(result.is_none());
}

#[tokio::test]
async fn timekeeper_propagates_read_errors() -> Result<(), foundationdb::FdbBindingError> {
    let database = foundationdb::Database::new_compat(None).await?;
    let trx = database.create_trx()?;
    trx.set_read_version(1);
    let error = hint_version_from_timestamp(&trx, 0, HintMode::AfterTimestamp, true)
        .await
        .expect_err("an expired read must not be treated as a missing timestamp");
    assert_eq!(error.get_fdb_error().unwrap().code(), 1007);
    Ok(())
}
