#[cfg(feature = "trace")]
#[test]
fn test_trace() {
    use tracing_subscriber::fmt::format::FmtSpan;

    let _guard = unsafe { foundationdb::boot() };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let test_writer = tracing_subscriber::fmt::TestWriter::new();

    let _e = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_span_events(FmtSpan::FULL)
        .with_writer(test_writer)
        .try_init();

    rt.block_on(async {
        test_traces_on_run().await;
    });
}

#[cfg(feature = "trace")]
#[tracing::instrument(skip_all)]
async fn test_traces_on_run() {
    let db = foundationdb::Database::new_compat(None)
        .await
        .expect("failed to open fdb");

    db.run(|trx, _b| async move {
        trx.set("tracing".as_ref(), "ok".as_ref());
        Ok(())
    })
    .await
    .expect("could not run transaction");

    // cleaning up
    db.run(|trx, _b| async move {
        trx.clear("tracing".as_ref());
        Ok(())
    })
    .await
    .expect("could not run transaction");
}
