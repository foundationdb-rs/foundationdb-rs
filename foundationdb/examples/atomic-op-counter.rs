use byteorder::ByteOrder;
use foundationdb::tuple::Subspace;
use foundationdb::{options, Database, FdbError, Transaction};

#[tokio::main]
async fn main() {
    let _guard = unsafe { foundationdb::boot() };
    let db = Database::new_compat(None)
        .await
        .expect("failed to get database");

    let counter_key = Subspace::all().subspace(&"stats").pack(&"my_counter");

    // write initial value
    let trx = db.create_trx().expect("could not create transaction");
    increment(&trx, &counter_key, 1);
    trx.commit().await.expect("could not commit");

    // read counter
    let trx = db.create_trx().expect("could not create transaction");
    let v1 = read_counter(&trx, &counter_key)
        .await
        .expect("could not read counter");
    dbg!(v1);
    assert!(v1 > 0);

    // decrement
    let trx = db.create_trx().expect("could not create transaction");
    increment(&trx, &counter_key, -1);
    trx.commit().await.expect("could not commit");

    let trx = db.create_trx().expect("could not create transaction");
    let v2 = read_counter(&trx, &counter_key)
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

async fn read_counter(trx: &Transaction, key: &[u8]) -> Result<i64, FdbError> {
    let raw_counter = trx
        .get(key, true)
        .await
        .expect("could not read key")
        .expect("no value found");

    let counter = byteorder::LE::read_i64(raw_counter.as_ref());
    Ok(counter)
}
