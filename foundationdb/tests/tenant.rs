mod common;

#[test]
fn test_tenant() {
    let _guard = unsafe { foundationdb::boot() };
    #[cfg(any(feature = "fdb-7_1",))]
    futures::executor::block_on(test_tenant_internal()).expect("failed to run");
}

#[cfg(any(feature = "fdb-7_1",))]
async fn test_tenant_internal() -> foundationdb::FdbResult<()> {
    use foundationdb::tenant::TenantManagement;

    let tenant = format!(
        "tenant-{:?}",
        std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    );
    let db = common::database().await?;
    TenantManagement::create_tenant(&db, tenant.as_bytes())
        .await
        .expect("could not create tenant");

    let tenant_info = TenantManagement::get_tenant(&db, tenant.as_bytes())
        .await
        .expect("could not retrieve tenant")
        .expect("tenant does not exist")
        .expect("tenant could not be deserialized");

    dbg!(&tenant_info);
    assert_eq!(
        &tenant_info.name,
        tenant.as_bytes(),
        "tenant name are not equals"
    );

    let tenants = TenantManagement::list_tenant(&db, "a".as_bytes(), "z".as_bytes(), None).await?;
    let tenants_size = tenants.len();
    assert!(!tenants.is_empty(), "received an empty list of tenants");

    TenantManagement::delete_tenant(&db, tenant.as_bytes())
        .await
        .expect("could not delete tenant");

    let tenants = TenantManagement::list_tenant(&db, "a".as_bytes(), "z".as_bytes(), None).await?;
    assert_eq!(
        tenants.len(),
        tenants_size - 1,
        "received a bad list of tenants"
    );
    Ok(())
}
