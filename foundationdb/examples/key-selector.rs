use std::ops::Deref;
use foundationdb::{tuple::{pack, unpack}, Database, FdbResult, KeySelector};
use tokio;

#[tokio::main]
async fn main() {
    let network = unsafe { foundationdb::boot() };

    run_key_selector_example()
        .await
        .expect("failed to run `run_key_selector_example`");

    run_key_selector_tuple_example()
        .await
        .expect("failed to run `run_key_selector_tuple_example`");

    drop(network);
}

async fn run_key_selector_example() -> FdbResult<()> {
    let db = Database::default()?;
    db.run(|trx, _maybe_committed| async move {
        trx.set(b"hello", b"world");
        trx.set(b"rust", b"is best");
        Ok(())
    }).await.unwrap();

    match db
        .run(|trx, _maybe_committed| async move {
            Ok(trx.get_key(&KeySelector::first_greater_than(b"hello"), true).await.unwrap())
        })
        .await
    {
        Ok(slice) => assert_eq!(b"rust", slice.as_ref()),
        Err(_) => eprintln!("cannot commit transaction"),
    }
    Ok(())
}

async fn run_key_selector_tuple_example() -> FdbResult<()> {
    let db = Database::default()?;

    db.run(|trx, _maybe_committed| async move {
        let key1 = pack(&("apple", 10));
        let key2 = pack(&("mango", 2));

        trx.set(&key1, &pack(&""));
        trx.set(&key2, &pack(&""));
        Ok(())
    }).await.unwrap();

    match db
        .run(|trx, _maybe_committed| async move {
            Ok(trx.get_key(&KeySelector::first_greater_than(&pack(&("apple", 5))), true).await.unwrap())
        })
        .await
    {
        Ok(slice) => {
            let next_key_tuple: (String, i64) = unpack(slice.deref()).expect("failed to unpack tuple");
            assert_eq!(("apple".to_string(), 10), next_key_tuple);
        }
        Err(_) => eprintln!("cannot commit transaction"),
    }

    match db
        .run(|trx, _maybe_committed| async move {
            Ok(trx.get_key(&KeySelector::last_less_or_equal(&pack(&("orange", 5))), true).await.unwrap())
        })
        .await
    {
        Ok(slice) => {
            let next_key_tuple: (String, i64) = unpack(slice.deref()).expect("failed to unpack tuple");
            assert_eq!(("mango".to_string(), 2), next_key_tuple);
        }
        Err(_) => eprintln!("cannot commit transaction"),
    }
    Ok(())
}
