use crate::common;
use foundationdb::timekeeper::hint_version_from_timestamp;
use std::cmp::{max, min};
use std::time::SystemTime;

#[tokio::test]
async fn timekeeper() {
    let _guard = unsafe { foundationdb::boot() };
    let database = common::database().await.expect("Unable to create database");
    let trx_read = database.create_trx().expect("Unable to create transaction");
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("Unable to get timestamp")
        .as_secs();
    let read_version = trx_read
        .get_read_version()
        .await
        .expect("Unable to get read version") as u64;
    // A new transaction is needed because the one which get the read version has no
    // knowledge about the new System Namespace Keyspace thus, further timekeeper
    // keys are unknown for the first transaction.
    // Creating a new one, make the first to be commited. The second will have the right
    // timekeeper state.
    let trx = database.create_trx().expect("Unable to create transaction");
    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    let result = hint_version_from_timestamp(&trx, now, false)
        .await
        .expect("Unable to get hint version");

    if let Some(hint_version) = result {
        // read version is roughly incremented by one million
        // we are safe if the delta between found read version
        // and expected one doesn't exceed 1e6 * 10 = 10 millions
        let max = max(hint_version, read_version);
        let min = min(hint_version, read_version);
        assert!(
            (max - min) < 10_000_000,
            "Found hint version {} but read version {} is too far away",
            hint_version,
            read_version
        )
    } else {
        panic!("No hint version found")
    }
}
