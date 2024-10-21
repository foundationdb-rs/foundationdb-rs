#[tokio::main]
async fn main() {
    foundationdb::boot().expect("could not boot fdb");

    // Have fun with the FDB API
    hello_world().await.expect("could not run the hello world");

    foundationdb::api::stop_network().expect("could not stop network");
}

async fn hello_world() -> foundationdb::FdbResult<()> {
    let db = foundationdb::Database::default()?;

    // write a value in a retryable closure
    match db
        .run(|trx, _maybe_committed| async move {
            trx.set(b"hello", b"world");
            Ok(())
        })
        .await
    {
        Ok(_) => println!("transaction committed"),
        Err(_) => eprintln!("cannot commit transaction"),
    };

    // read a value
    match db
        .run(|trx, _maybe_committed| async move { Ok(trx.get(b"hello", false).await.unwrap()) })
        .await
    {
        Ok(slice) => assert_eq!(b"world", slice.unwrap().as_ref()),
        Err(_) => eprintln!("cannot commit transaction"),
    }

    Ok(())
}
