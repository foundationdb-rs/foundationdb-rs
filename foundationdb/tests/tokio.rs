use foundationdb::*;
use futures::prelude::*;
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
        do_trx().await;
    });
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
