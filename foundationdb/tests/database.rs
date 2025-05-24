use foundationdb_macros::cfg_api_versions;

mod common;

#[tokio::test]
async fn test_databse() {
    let _guard = unsafe { foundationdb::boot() };

    #[cfg(feature = "fdb-7_3")]
    test_status_async().await.expect("failed to run");

    #[cfg(any(feature = "fdb-7_1", feature = "fdb-7_3"))]
    test_get_main_thread_busyness_async()
        .await
        .expect("failed to get busyness");
}

#[cfg_api_versions(min = 730)]
async fn test_status_async() -> foundationdb::FdbResult<()> {
    let db = common::database().await?;
    let status = db.get_client_status().await?;

    let status =
        std::str::from_utf8(status.as_ref()).expect("could not find any utf-8 bytes to read");
    assert!(
        status.contains("Healthy"),
        "Could not find healthy in '{}'",
        status
    );

    Ok(())
}

#[cfg_api_versions(min = 710)]
async fn test_get_main_thread_busyness_async() -> foundationdb::FdbResult<()> {
    let db = common::database().await?;

    let busyness = db
        .get_main_thread_busyness()
        .await
        .expect("could not get busyness");
    assert!(
        busyness >= 0.0,
        "{}",
        format!("negative thread busyness: {}", busyness)
    );
    Ok(())
}
