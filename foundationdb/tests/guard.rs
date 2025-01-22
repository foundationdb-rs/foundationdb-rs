use foundationdb::api::NetworkAutoStop;
use foundationdb::{Database, FdbResult};
use std::time::Duration;
use tokio::time::sleep;

mod common;

#[tokio::test]
async fn test_guard_1() {
    // Ensure that the very first step of the test is to acquire a NetworkAutoStop
    let guard = NetworkAutoStop::new();

    // Then you can boot normally, and skip the NetworkAutoStop provided by boot
    unsafe {
        foundationdb::boot().expect("could not boot fdb");
    }

    let db = common::database().await.expect("could not get database");

    do_some_work(&db).await.expect("could not do some work");

    drop(guard);
}

async fn do_some_work(db: &Database) -> FdbResult<()> {
    for _ in 0..10 {
        db.perform_no_op().await?;
        sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}

#[tokio::test]
async fn test_guard_2() {
    // Ensure that the very first step of the test is to acquire a NetworkAutoStop
    let guard = NetworkAutoStop::new();

    // Then you can boot normally, and skip the NetworkAutoStop provided by boot
    unsafe {
        foundationdb::boot().expect("could not boot fdb");
    }

    let db = common::database().await.expect("could not get database");

    do_some_work(&db).await.expect("could not do some work");

    drop(guard);
}

#[tokio::test]
async fn test_guard_3() {
    // Ensure that the very first step of the test is to acquire a NetworkAutoStop
    let guard = NetworkAutoStop::new();

    // Then you can boot normally, and skip the NetworkAutoStop provided by boot
    unsafe {
        foundationdb::boot().expect("could not boot fdb");
    }

    let db = common::database().await.expect("could not get database");

    do_some_work(&db).await.expect("could not do some work");

    drop(guard);
}
