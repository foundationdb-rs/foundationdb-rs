use foundationdb_macros::cfg_api_versions;

mod common;

// #[test]
// fn test_databse() {
//     let _guard = unsafe { foundationdb::boot() };

//     if_cfg_api_versions!(min = 730 =>
//         futures::executor::block_on(test_status_async()).expect("failed to run")
//     );

//     if_cfg_api_versions!(min = 710 =>
//         futures::executor::block_on(test_get_main_thread_busyness_async())
//             .expect("failed to get busyness")
//     );
// }

#[cfg_api_versions(min = 730)]
#[tokio::test]
async fn test_status_async() -> foundationdb::FdbResult<()> {
    let db = common::database().await?;
    let status = db.get_client_status().await?;

    let status =
        std::str::from_utf8(status.as_ref()).expect("could not find any utf-8 bytes to read");
    assert!(
        status.contains("Healthy"),
        "Could not find healthy in '{status}'"
    );

    Ok(())
}

#[cfg_api_versions(min = 710)]
#[tokio::test]
async fn test_get_main_thread_busyness_async() -> foundationdb::FdbResult<()> {
    let db = common::database().await?;

    let busyness = db
        .get_main_thread_busyness()
        .await
        .expect("could not get busyness");
    assert!(
        busyness == 0.0,
        "{}",
        format!("non-zero thread busyness: {busyness}")
    );
    Ok(())
}
