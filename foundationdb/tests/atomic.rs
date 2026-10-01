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

fn set_test_options(trx: &Transaction) -> FdbResult<()> {
    trx.set_option(options::TransactionOption::RetryLimit(10))?;
    trx.set_option(options::TransactionOption::Timeout(30_000))
}

async fn atomic_add(db: &Database, key: &[u8], value: i64) -> FdbResult<()> {
    db.transact_boxed(
        (key, value.to_le_bytes()),
        |trx, (key, value)| {
            async move {
                set_test_options(trx)?;
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
