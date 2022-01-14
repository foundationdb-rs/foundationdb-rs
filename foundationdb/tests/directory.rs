// Copyright 2018 foundationdb-rs developers, https://github.com/Clikengo/foundationdb-rs/graphs/contributors
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

use foundationdb::directory::directory_layer::DirectoryLayer;

use foundationdb::directory::Directory;

use foundationdb::*;

mod common;

#[test]
// testing basic features of the Directory, everything is tracked using with the BindingTester.
fn test_directory() {
    let _guard = unsafe { foundationdb::boot() };
    let db = futures::executor::block_on(common::database()).expect("cannot open fdb");

    eprintln!("clearing all keys");
    let trx = db.create_trx().expect("cannot create txn");
    trx.clear_range(b"", b"\xff");
    futures::executor::block_on(trx.commit()).expect("could not clear keys");

    eprintln!("creating directories");
    let directory = DirectoryLayer::default();

    futures::executor::block_on(test_create_then_open_then_delete(
        &db,
        &directory,
        vec![String::from("application")],
    ))
    .expect("failed to run");

    futures::executor::block_on(test_create_then_open_then_delete(
        &db,
        &directory,
        vec![String::from("1"), String::from("2")],
    ))
    .expect("failed to run");
}

async fn test_create_then_open_then_delete(
    db: &Database,
    directory: &DirectoryLayer,
    paths: Vec<String>,
) -> FdbResult<()> {
    let trx = db.create_trx()?;

    eprintln!("creating {:?}", &paths);
    let create_output = directory.create(&trx, paths.to_owned(), None, None).await;
    assert!(
        create_output.is_ok(),
        "cannot create: {:?}",
        create_output.err().unwrap()
    );
    trx.commit().await.expect("cannot commit");
    let trx = db.create_trx()?;

    eprintln!("opening {:?}", &paths);
    let open_output = directory.open(&trx, paths.to_owned(), None).await;
    assert!(
        open_output.is_ok(),
        "cannot create: {:?}",
        open_output.err().unwrap()
    );

    assert_eq!(create_output.unwrap().bytes(), open_output.unwrap().bytes());
    trx.commit().await.expect("cannot commit");

    // removing folder
    Ok(())
}
