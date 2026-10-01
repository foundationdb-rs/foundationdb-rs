// Copyright 2018 foundationdb-rs developers, https://github.com/Clikengo/foundationdb-rs/graphs/contributors
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.
use byteorder::ByteOrder;
use foundationdb::*;
use futures::FutureExt;
use futures::future::*;

mod common;

async fn atomic_add(db: &Database, key: &[u8], value: i64) -> FdbResult<()> {
    atomic_add_with(db, key, value, |_| Ok(())).await
}

fn set_test_options(trx: &Transaction) -> FdbResult<()> {
    trx.set_option(options::TransactionOption::RetryLimit(10))?;
    trx.set_option(options::TransactionOption::Timeout(30_000))
}

// The preparation hook lets the regression below expire an actual commit
// without depending on how quickly the test machine executes the workload.
async fn atomic_add_with<F>(db: &Database, key: &[u8], value: i64, prepare: F) -> FdbResult<()>
where
    F: FnMut(&Transaction) -> FdbResult<()> + Send,
{
    db.transact_boxed(
        (key, value.to_le_bytes(), prepare),
        |trx, (key, value, prepare)| {
            async move {
                set_test_options(trx)?;
                prepare(trx)?;
                trx.atomic_op(key, value, options::MutationType::Add);
                Ok(())
            }
            .boxed()
        },
        // Atomic addition is not idempotent: retry definite failures, but
        // propagate commit_unknown_result instead of possibly adding twice.
        TransactOption::default(),
    )
    .await
}

async fn clear_key(db: &Database, key: &[u8]) -> FdbResult<()> {
    db.transact_boxed(
        key,
        |trx, key| {
            async move {
                set_test_options(trx)?;
                trx.clear(key);
                Ok(())
            }
            .boxed()
        },
        TransactOption::default(),
    )
    .await
}

async fn read_counter(db: &Database, key: &[u8]) -> FdbResult<i64> {
    db.transact_boxed(
        key,
        |trx, key| {
            async move {
                set_test_options(trx)?;
                let value = trx.get(key, false).await?.expect("value should exist");
                Ok(byteorder::LE::read_i64(&value))
            }
            .boxed()
        },
        TransactOption::default(),
    )
    .await
}

#[tokio::test]
async fn test_atomic() -> FdbResult<()> {
    const KEY: &[u8] = b"test-atomic";

    let db = common::database().await?;

    println!("clear!");
    clear_key(&db, KEY).await?;

    println!("concurrent!");
    {
        let n = 1000usize;

        let fut_add = try_join_all((0..n).map(|_| atomic_add(&db, KEY, 1)));
        let fut_sub = try_join_all((0..n).map(|_| atomic_add(&db, KEY, -1)));

        // Wait for all atomic operations
        try_join(fut_add, fut_sub).await?;
    }

    println!("check!");
    assert_eq!(read_counter(&db, KEY).await?, 0);
    Ok(())
}

#[tokio::test]
async fn test_atomic_retries_expired_commit() -> FdbResult<()> {
    const KEY: &[u8] = b"test-atomic-expired";
    const END: &[u8] = b"test-atomic-expired\x00";
    let db = common::database().await?;
    clear_key(&db, KEY).await?;
    atomic_add(&db, KEY, 0).await?;

    let expire = |trx: &Transaction| {
        trx.set_read_version(1);
        trx.add_conflict_range(KEY, END, options::ConflictRangeType::Read)
    };

    // Prove that the injected stale read version makes a native commit fail.
    let trx = db.create_trx()?;
    set_test_options(&trx)?;
    expire(&trx)?;
    trx.atomic_op(KEY, &1i64.to_le_bytes(), options::MutationType::Add);
    let error = match trx.commit().await {
        Err(error) => error,
        Ok(_) => panic!("stale commit unexpectedly succeeded"),
    };
    assert_eq!(error.code(), 1007);

    let mut attempts = 0;
    atomic_add_with(&db, KEY, 1, |trx| {
        attempts += 1;
        if attempts == 1 {
            expire(trx)?;
        }
        Ok(())
    })
    .await?;
    assert!(attempts >= 2, "the expired commit must be retried");
    assert_eq!(read_counter(&db, KEY).await?, 1);
    Ok(())
}

#[tokio::test]
async fn test_atomic_retry_policy() -> FdbResult<()> {
    const KEY: &[u8] = b"test-atomic-retry-policy";
    let db = common::database().await?;
    clear_key(&db, KEY).await?;
    atomic_add(&db, KEY, 0).await?;

    let mut attempts = 0;
    let error = atomic_add_with(&db, KEY, 1, |_| {
        attempts += 1;
        Err(FdbError::from_code(1021))
    })
    .await
    .expect_err("an ambiguous commit must not be replayed");
    assert_eq!(error.code(), 1021);
    assert_eq!(attempts, 1);

    attempts = 0;
    let error = atomic_add_with(&db, KEY, 1, |_| {
        attempts += 1;
        Err(FdbError::from_code(1007))
    })
    .await
    .expect_err("persistent failures must exhaust the retry budget");
    assert_eq!(error.code(), 1007);
    assert_eq!(attempts, 11);
    assert_eq!(read_counter(&db, KEY).await?, 0);
    Ok(())
}
