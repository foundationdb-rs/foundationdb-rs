// Copyright 2018 foundationdb-rs developers, https://github.com/Clikengo/foundationdb-rs/graphs/contributors
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

use foundationdb::{
    options::MutationType,
    tuple::{Subspace, Versionstamp},
    Database,
};
use futures::StreamExt;

mod common;

#[test]
// testing subspace with versionstamps.
fn test_tuples() {
    let _guard = unsafe { foundationdb::boot() };
    let db = futures::executor::block_on(common::database()).expect("cannot open fdb");

    eprintln!("clearing all keys");
    let trx = db.create_trx().expect("cannot create txn");
    trx.clear_range(b"", b"\xff");
    futures::executor::block_on(trx.commit()).expect("could not clear keys");

    eprintln!("creating directories");
    futures::executor::block_on(test_subspace_with_versionstamp(&db));
}

async fn test_subspace_with_versionstamp(db: &Database) {
    let trx = db.create_trx().expect("cannot create txn");

    // In this example we will create a subspace starting with a versionstamp.
    let subspace = Subspace::from("root");
    let subspace = subspace.subspace(&Versionstamp::incomplete(0));
    let key = subspace.pack_with_versionstamp(&"key");

    eprintln!("writing key {key:?}");
    trx.atomic_op(&key, b"hello", MutationType::SetVersionstampedKey);

    // we want to get the versionstamp back to be able to read the subspace.
    let versionstamp = trx.get_versionstamp();

    trx.commit().await.expect("cannot commit");

    let versionstamp = versionstamp
        .await
        .expect("cannot get versionstamp after commit");
    let versionstamp = Versionstamp::complete(
        (*versionstamp)
            .try_into()
            .expect("versionstamp is the wrong size"),
        0,
    );
    // Now that we have the versionstamp we can rebuild the subspace and read the key.
    let trx = db.create_trx().expect("cannot create txn");

    // In this example we will create a subspace starting with a versionstamp.
    let subspace = Subspace::from("root");
    let subspace = subspace.subspace(&versionstamp);

    {
        // we can read the whole subspace to get the chance to unpack the key too.
        let mut elements = trx.get_ranges_keyvalues(subspace.range().into(), false);
        while let Some(key_value) = elements.next().await {
            let key_value = key_value.expect("cannot read keyvalue");
            let key = subspace
                .unpack::<String>(key_value.key())
                .expect("cannot unpack key");
            assert_eq!(key, "key");
            assert_eq!(key_value.value(), b"hello");
        }
    }
    trx.commit().await.expect("cannot commit");

    // Now that the subspace already exists, we can add more versionstamped keys in there.
    let trx = db.create_trx().expect("cannot create txn");
    let key = subspace.pack_with_versionstamp(&Versionstamp::incomplete(0));
    trx.atomic_op(&key, b"hello2", MutationType::SetVersionstampedKey);
    let key_versionstamp = trx.get_versionstamp();
    trx.commit().await.expect("cannot commit");
    let key_versionstamp = key_versionstamp
        .await
        .expect("cannot get versionstamp after commit");
    let key_versionstamp = Versionstamp::complete(
        (*key_versionstamp)
            .try_into()
            .expect("versionstamp is the wrong size"),
        0,
    );
    // we can read the key back by re-packing with the subspace:
    let trx = db.create_trx().expect("cannot create txn");
    let key = subspace.pack_with_versionstamp(&key_versionstamp);
    let value = trx.get(&key, false).await.expect("cannot read key");
    assert_eq!(value.as_deref(), Some(b"hello2".as_ref()));
    trx.commit().await.expect("cannot commit");
}
