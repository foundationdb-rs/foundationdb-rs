#[foundationdb_macros::cfg_api_versions(min = 610)]
mod external_api {
    use foundationdb::{
        Database,
        options::MutationType,
        tuple::{Subspace, Versionstamp},
    };
    use foundationdb_sys as sys;
    use std::{ptr::NonNull, thread};

    struct ExternalNetwork(Option<thread::JoinHandle<i32>>);

    impl Drop for ExternalNetwork {
        fn drop(&mut self) {
            let stop_result = unsafe { sys::fdb_stop_network() };
            let run_result = self.0.take().unwrap().join();
            if !thread::panicking() {
                assert_eq!(stop_result, 0, "stop external network");
                assert_eq!(run_result.expect("join external network"), 0);
            }
        }
    }

    // This process deliberately never initializes the client through the Rust API.
    #[tokio::test]
    async fn versionstamped_mutations_support_external_initialization() {
        unsafe {
            assert_eq!(
                sys::fdb_select_api_version_impl(
                    sys::FDB_API_VERSION as i32,
                    sys::FDB_API_VERSION as i32,
                ),
                0,
                "select API through the C API"
            );
            assert_eq!(sys::fdb_setup_network(), 0, "set up external network");
        }
        let _network = ExternalNetwork(Some(thread::spawn(|| unsafe { sys::fdb_run_network() })));
        let mut raw_db = std::ptr::null_mut();
        unsafe {
            assert_eq!(
                sys::fdb_create_database(std::ptr::null(), &mut raw_db),
                0,
                "open database through the C API"
            );
        }
        let db = unsafe { Database::new_from_pointer(NonNull::new(raw_db).unwrap()) };
        let subspace = Subspace::from((
            "test-versionstamp-external-api",
            uuid::Uuid::new_v4().to_string(),
        ));
        let key = subspace.pack_with_versionstamp(&("key", Versionstamp::incomplete(7)));
        let value_key = subspace.pack(&"value");
        let mut raw_value = b"prefix".to_vec();
        let value_offset = raw_value.len();
        raw_value.extend_from_slice(&[0xff; 10]);
        raw_value.extend_from_slice(b"suffix");
        raw_value.extend_from_slice(&u32::try_from(value_offset).unwrap().to_le_bytes());

        let trx = db.create_trx().expect("create transaction");
        trx.atomic_op(&key, b"external", MutationType::SetVersionstampedKey);
        trx.atomic_op(&value_key, &raw_value, MutationType::SetVersionstampedValue);
        let committed_stamp = trx.get_versionstamp();
        trx.commit()
            .await
            .expect("commit externally initialized mutations");
        let committed_stamp = committed_stamp.await.expect("read committed versionstamp");
        let complete = Versionstamp::complete((*committed_stamp).try_into().unwrap(), 7);
        let expected_key = subspace.pack(&("key", complete));
        raw_value.truncate(raw_value.len() - 4);
        raw_value[value_offset..value_offset + 10].copy_from_slice(&committed_stamp);

        let trx = db.create_trx().expect("create read transaction");
        let value = trx
            .get(&expected_key, false)
            .await
            .expect("read versionstamped key");
        assert_eq!(value.as_deref(), Some(b"external".as_slice()));
        let value = trx
            .get(&value_key, false)
            .await
            .expect("read versionstamped value");
        assert_eq!(value.as_deref(), Some(raw_value.as_slice()));
        trx.clear_subspace_range(&subspace);
        trx.commit().await.expect("clean up test subspace");
    }
}
