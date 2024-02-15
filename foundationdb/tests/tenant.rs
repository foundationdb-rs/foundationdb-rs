mod common;

#[test]
fn test_tenant() {
    let _guard = unsafe { foundationdb::boot() };
    #[cfg(all(
        any(feature = "fdb-7_1", feature = "fdb-7_3"),
        feature = "tenant-experimental"
    ))]
    {
        futures::executor::block_on(test_tenant_management()).expect("failed to run");
        futures::executor::block_on(test_tenant_run()).expect("failed to run");
    }
}

#[cfg(all(
    any(feature = "fdb-7_1", feature = "fdb-7_3"),
    feature = "tenant-experimental"
))]
async fn test_tenant_management() -> foundationdb::FdbResult<()> {
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
    #[cfg(feature = "fdb-7_1")]
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

#[cfg(all(
    any(feature = "fdb-7_1", feature = "fdb-7_3"),
    feature = "tenant-experimental"
))]
async fn test_tenant_run() -> foundationdb::FdbResult<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let db = common::database().await?;
    // write the same key in multiple tenants
    for tenant_id in 0..10 {
        let tenant_name = format!("tenant-{}-{}", now, tenant_id);
        let tenant_name_ref = &tenant_name;

        foundationdb::tenant::TenantManagement::create_tenant(&db, tenant_name.as_bytes())
            .await
            .expect("could not create tenant");

        // open tenant
        let tenant = db
            .open_tenant(tenant_name.as_bytes())
            .expect("could not open tenant");

        // write a key into it
        tenant
            .run(|trx, _maybe_committed| async move {
                trx.set("toto".as_bytes(), tenant_name_ref.as_bytes());
                Ok(())
            })
            .await
            .expect("could not write key 'toto'");
    }

    // check that all keys are defined in multiple tenants
    for tenant_id in 0..10 {
        let tenant_name = format!("tenant-{}-{}", now, tenant_id);

        let tenant = db
            .open_tenant(tenant_name.as_bytes())
            .expect("could not open tenant");

        let slice = tenant
            .run(|trx, _maybe_committed| async move {
                Ok(trx
                    .get("toto".as_bytes(), false)
                    .await?
                    .expect("could not find key 'toto'"))
            })
            .await
            .expect("could not read key 'toto'");

        assert_eq!(slice.as_ref(), tenant_name.as_bytes());

        // remove key so that we can delete tenant
        tenant
            .run(|trx, _maybe_committed| async move {
                trx.clear("toto".as_bytes());
                Ok(())
            })
            .await
            .expect("could not delete key 'toto'");

        foundationdb::tenant::TenantManagement::delete_tenant(&db, tenant_name.as_bytes())
            .await
            .expect("could not delete tenant");
    }

    Ok(())
}
