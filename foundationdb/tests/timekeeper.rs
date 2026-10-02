use foundationdb::timekeeper::{HintMode, hint_version_from_timestamp};
use std::time::SystemTime;

#[tokio::test]
async fn timekeeper() {
    let database = foundationdb::Database::new_compat(None)
        .await
        .expect("Unable to create database");
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("Unable to get timestamp")
        .as_secs();
    // Let some time passed in order to create a new timekeeper entry
    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    // A new transaction is needed because the one which get the read version has no
    // knowledge about the new System Namespace Keyspace thus, further timekeeper
    // keys are unknown for the first transaction.
    // Creating a new one, make the first to be commited. The second will have the right
    // timekeeper state.
    let trx = database.create_trx().expect("Unable to create transaction");
    let result = hint_version_from_timestamp(&trx, now, HintMode::AfterTimestamp, true)
        .await
        .expect("Unable to get hint version");
    assert!(result.is_some());

    // create a new transaction to fail getting read version in the future
    let trx = database.create_trx().expect("Unable to create transaction");
    let future_date = now + 50;
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
    let future_date = now + 50;
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
