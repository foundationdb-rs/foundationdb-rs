extern crate core;

use foundationdb::future::FdbValues;
use foundationdb::options::ConflictRangeType;
use foundationdb::tuple::{pack, Subspace};
use foundationdb::*;
use futures::prelude::*;
use std::fmt::{Display, Formatter};
use std::sync::atomic::{AtomicI16, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Runtime;
use tokio::time::timeout;

mod common;

#[test]
fn test_tokio_send() {
    let _guard = unsafe { foundationdb::boot() };
    let rt = Runtime::new().unwrap();
    rt.block_on(async {
        do_transact().await;
        do_run().await;
        do_run_with_transaction_limits().await;
        do_trx().await;
        do_run_with_custom_error().await;
    });
}

#[derive(Debug)]
enum CustomError {
    MyError1,
}

impl Display for CustomError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "MyError1")
    }
}

impl std::error::Error for CustomError {}

async fn do_run_with_custom_error() {
    let db = Arc::new(
        foundationdb::Database::new_compat(None)
            .await
            .expect("failed to open fdb"),
    );

    db.perform_no_op().await.expect("could not perform no_op");

    let result: Result<(), FdbBindingError> = db
        .run(|_trx, _maybe_committed| async move {
            let toto: Box<CustomError> = Box::new(CustomError::MyError1);
            Err(FdbBindingError::new_custom_error(toto))
        })
        .await;

    match result {
        Ok(_) => panic!("expecting an error, got an Ok from db.run"),
        Err(err) => match err {
            FdbBindingError::CustomError(e) => match e.downcast_ref::<CustomError>().unwrap() {
                // no-op
                CustomError::MyError1 => {}
            },
            FdbBindingError::ReferenceToTransactionKept => {}
            _ => panic!("expecting an Custom Error"),
        },
    }
}

async fn do_transact() {
    let db = Arc::new(
        foundationdb::Database::new_compat(None)
            .await
            .expect("failed to open fdb"),
    );

    if (timeout(Duration::from_secs(1), db.perform_no_op()).await).is_err() {
        panic!("database is unavailable");
    }

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
        .run(|trx, _maybe_committed| async move {
            let read_subspace = Subspace::all().subspace(&("do_run"));
            let conflict_transaction = db_arc.create_trx().expect("could not create a transaction");

            let local_counter = counter_ref.fetch_add(1, Ordering::SeqCst);

            // virtually reading some random data in the subspace "do_run"
            trx.add_conflict_range(
                &read_subspace.range().0,
                &read_subspace.range().1,
                ConflictRangeType::Write,
            )?;

            let _keys: Vec<FdbValues> = trx
                .get_ranges(read_subspace.range().into(), false)
                .try_collect()
                .await?;

            trx.set(&read_subspace.pack(&(1)), &pack(&("42")));

            if local_counter < limit_retry {
                // writing with another transaction in the subspace "do_run"
                conflict_transaction.set(&read_subspace.pack(&(2)), &pack(&("42")));
                conflict_transaction
                    .commit()
                    .await
                    .expect("cannot commit the conflict transaction");
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
        .run(|trx, _maybe_committed| async move {
            let read_subspace = Subspace::all().subspace(&("do_run"));
            trx.set_option(foundationdb::options::TransactionOption::RetryLimit(16))?;

            let conflict_transaction = db_arc.create_trx().expect("could not create a transaction");
            counter_ref.fetch_add(1, Ordering::SeqCst);

            // virtually reading some random data in the subspace "do_run"
            let _keys: Vec<FdbValues> = trx
                .get_ranges(read_subspace.range().into(), false)
                .try_collect()
                .await?;

            trx.set(&Subspace::all().pack(&("do_run", "1")), &pack(&("42")));

            // writing with another transaction in the subspace "do_run"
            conflict_transaction.set(&Subspace::all().pack(&("do_run", "2")), &pack(&("42")));
            conflict_transaction
                .commit()
                .await
                .expect("cannot commit the conflict transaction");

            Ok(())
        })
        .await;

    assert!(result.is_err());
    assert_eq!(limit_retry + 1, counter.load(Ordering::SeqCst));
}
