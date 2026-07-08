use byteorder::ByteOrder;
use foundationdb::tuple::Subspace;
use foundationdb::{Database, FdbResult, Transaction, options};

#[tokio::main]
async fn main() {
    foundationdb::boot().expect("failed to initialize FoundationDB");
    let db = Database::new_compat(None)
        .await
        .expect("failed to get database");
    db.set_option(options::DatabaseOption::TransactionTimeout(5000))
        .expect("failed to set transaction timeout");
    db.set_option(options::DatabaseOption::TransactionRetryLimit(3))
        .expect("failed to set transaction retry limit");

    let counter_key = Subspace::all().subspace(&"stats").pack(&"my_counter");

    // write initial value
    db.run(|trx, _maybe_committed| {
        let counter_key = counter_key.clone();
        async move {
            increment(&trx, &counter_key, 1);
            Ok(())
        }
    })
    .await
    .expect("could not commit");

    // read counter
    let v1 = db
        .run(|trx, _maybe_committed| {
            let counter_key = counter_key.clone();
            async move {
                let val = read_counter(&trx, &counter_key).await?;
                Ok(val)
            }
        })
        .await
        .expect("could not read counter");
    dbg!(v1);
    assert!(v1 > 0);

    // decrement
    db.run(|trx, _maybe_committed| {
        let counter_key = counter_key.clone();
        async move {
            increment(&trx, &counter_key, -1);
            Ok(())
        }
    })
    .await
    .expect("could not commit");

    let v2 = db
        .run(|trx, _maybe_committed| {
            let counter_key = counter_key.clone();
            async move {
                let val = read_counter(&trx, &counter_key).await?;
                Ok(val)
            }
        })
        .await
        .expect("could not read counter");
    dbg!(v2);
    assert_eq!(v1 - 1, v2);
}

fn increment(trx: &Transaction, key: &[u8], incr: i64) {
    // generate the right buffer for atomic_op
    let mut buf = [0u8; 8];
    byteorder::LE::write_i64(&mut buf, incr);

    trx.atomic_op(key, &buf, options::MutationType::Add);
}

async fn read_counter(trx: &Transaction, key: &[u8]) -> FdbResult<i64> {
    let raw_counter = trx.get(key, true).await?.expect("no value found");

    let counter = byteorder::LE::read_i64(raw_counter.as_ref());
    Ok(counter)
}
