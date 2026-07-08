#[tokio::main]
async fn main() {
    // The FoundationDB client is initialized on first use and stopped
    // automatically at process exit.
    hello_world().await.expect("could not run the hello world");
}

async fn hello_world() -> foundationdb::FdbResult<()> {
    let db = foundationdb::Database::default()?;

    // By default, the FoundationDB C API will retry indefinitely if it cannot reach the cluster
    // or if DNS resolution fails. To prevent this, you can set a timeout or a retry limit on the
    // database object.
    db.set_option(foundationdb::options::DatabaseOption::TransactionTimeout(
        5000,
    ))?; // 5 seconds
    db.set_option(foundationdb::options::DatabaseOption::TransactionRetryLimit(3))?;

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
