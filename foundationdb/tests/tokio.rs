use foundationdb::tuple::{pack, Subspace};
use foundationdb::*;
use futures::prelude::*;

use std::sync::atomic::{AtomicI16, Ordering};
use std::sync::Arc;
use tokio::runtime::Runtime;

mod common;

#[test]
fn test_tokio_send() {
    let _guard = unsafe { foundationdb::boot() };
    let rt = Runtime::new().unwrap();
    rt.block_on(async {
        do_run().await;
        do_run_with_transaction_limits().await;
        do_transact().await;
        do_trx().await;
    });
}

async fn do_run() {
    let limit_retry = 16;
    let db = Arc::new(
        foundationdb::Database::new_compat(None)
            .await
            .expect("failed to open fdb"),
    );

    let counter = AtomicI16::new(0);
    let counter_ref = &counter;
    let db_arc = &db;

    let result = db
        .run(|trx| async move {
            let conflict_transaction = db_arc.create_trx().expect("could not create a transaction");

            let local_counter = counter_ref.fetch_add(1, Ordering::SeqCst);

            // reading some random data in the subspace "do_run"
            trx.get_range(
                &RangeOption::from(&Subspace::all().subspace(&("do_run"))),
                1024,
                false,
            )
            .await?;
            trx.set(&Subspace::all().pack(&("do_run", "1")), &pack(&("42")));

            if local_counter < limit_retry {
                // writing with another transaction in the subspace "do_run"
                conflict_transaction.set(&Subspace::all().pack(&("do_run", "2")), &pack(&("42")));
                conflict_transaction.commit().await?;
            }

            // trying to commit
            Ok(())
        })
        .await;

    assert!(result.is_ok());
    assert_eq!(limit_retry + 1, counter.load(Ordering::SeqCst));
}

async fn do_run_with_transaction_limits() {
    let limit_retry = 16;
    let db = Arc::new(
        foundationdb::Database::new_compat(None)
            .await
            .expect("failed to open fdb"),
    );

    let db_arc = &db;
    let counter = AtomicI16::new(0);
    let counter_ref = &counter;

    let result = db
        .run(|trx| async move {
            trx.set_option(foundationdb::options::TransactionOption::RetryLimit(16))?;

            let conflict_transaction = db_arc.create_trx().expect("could not create a transaction");
            counter_ref.fetch_add(1, Ordering::SeqCst);

            // reading some random data in the subspace "do_run"
            trx.get_range(
                &RangeOption::from(&Subspace::all().subspace(&("do_run"))),
                1024,
                false,
            )
            .await?;
            trx.set(&Subspace::all().pack(&("do_run", "1")), &pack(&("42")));

            // writing with another transaction in the subspace "do_run"
            conflict_transaction.set(&Subspace::all().pack(&("do_run", "2")), &pack(&("42")));
            conflict_transaction.commit().await?;

            Ok(())
        })
        .await;

    assert!(result.is_err());
    assert_eq!(limit_retry + 1, counter.load(Ordering::SeqCst));
}

async fn do_transact() {
    let db = Arc::new(
        foundationdb::Database::new_compat(None)
            .await
            .expect("failed to open fdb"),
    );

    let adb = db;
    tokio::spawn(async move {
        async fn txnfn(_txn: &Transaction) -> FdbResult<()> {
            Ok(())
        }

        adb.transact_boxed(
            (),
            |txn: &Transaction, ()| txnfn(txn).boxed(),
            TransactOption::default(),
        )
        .await
        .expect("failed to transact")
    });
}

async fn do_trx() {
    let db = Arc::new(
        foundationdb::Database::new_compat(None)
            .await
            .expect("failed to open fdb"),
    );

    let adb = db;
    tokio::spawn(async move {
        adb.create_trx()
            .expect("failed to create trx")
            .commit()
            .await
            .expect("failed to commit");
    });
}
