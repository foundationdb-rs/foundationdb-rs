use foundationdb_macros::cfg_api_versions;

mod common;

#[cfg_api_versions(min = 730)]
#[test]
fn test_get_client_status() {
    let _guard = unsafe { foundationdb::boot() };
    futures::executor::block_on(test_status_async()).expect("failed to run");
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
        format!("{}", status)
    );

    Ok(())
}
