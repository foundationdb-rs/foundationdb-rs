use foundationdb::{
    Database,
    api::FdbApiBuilder,
    options::MutationType,
    tuple::{self, Subspace, Versionstamp},
};

// API selection is process-global, so this regression has its own test binary.
#[tokio::test]
async fn versionstamped_mutations_support_runtime_api_510() {
    FdbApiBuilder::default()
        .set_runtime_version(510)
        .build()
        .expect("select API 510");
    let db = Database::new_compat(None).await.expect("open database");
    let run_id = uuid::Uuid::new_v4().to_string();
    let prefix = ("test-versionstamp-api-510", run_id.as_str());
    let subspace = Subspace::from(prefix);
    let incomplete = Versionstamp::incomplete(42);

    let tuple_key =
        tuple::pack_with_versionstamp_for_key(&(prefix.0, prefix.1, "tuple", &incomplete), 510);
    let subspace_key = subspace.pack_with_versionstamp_for_key(&("subspace", &incomplete), 510);

    // Preserve the raw API's support for correctly encoded legacy operands.
    let mut raw_key = subspace.pack(&"raw-key");
    let raw_offset = raw_key.len();
    raw_key.extend_from_slice(&[0xff; 10]);
    raw_key.extend_from_slice(b"suffix");
    raw_key.extend_from_slice(&u16::try_from(raw_offset).unwrap().to_le_bytes());
    let value_key = subspace.pack(&"raw-value");
    let mut raw_value = vec![0xff; 10];
    raw_value.extend_from_slice(b"legacy-value");

    let trx = db.create_trx().expect("create transaction");
    trx.atomic_op(&tuple_key, b"tuple", MutationType::SetVersionstampedKey);
    trx.atomic_op(
        &subspace_key,
        b"subspace",
        MutationType::SetVersionstampedKey,
    );
    trx.atomic_op(&raw_key, b"raw-key", MutationType::SetVersionstampedKey);
    trx.atomic_op(&value_key, &raw_value, MutationType::SetVersionstampedValue);
    let committed_stamp = trx.get_versionstamp();
    trx.commit().await.expect("commit API 510 mutations");
    let committed_stamp = committed_stamp.await.expect("read committed versionstamp");
    let complete = Versionstamp::complete((*committed_stamp).try_into().unwrap(), 42);

    let expected_tuple_key = tuple::pack(&(prefix.0, prefix.1, "tuple", &complete));
    let expected_subspace_key = subspace.pack(&("subspace", &complete));
    raw_key.truncate(raw_key.len() - 2);
    raw_key[raw_offset..raw_offset + 10].copy_from_slice(&committed_stamp);
    raw_value[..10].copy_from_slice(&committed_stamp);

    let trx = db.create_trx().expect("create read transaction");
    for (key, expected) in [
        (expected_tuple_key, b"tuple".as_slice()),
        (expected_subspace_key, b"subspace".as_slice()),
        (raw_key, b"raw-key".as_slice()),
        (value_key, raw_value.as_slice()),
    ] {
        let value = trx
            .get(&key, false)
            .await
            .expect("read versionstamped data");
        assert_eq!(value.as_deref(), Some(expected));
    }
    trx.clear_subspace_range(&subspace);
    trx.commit().await.expect("clean up test subspace");
}
