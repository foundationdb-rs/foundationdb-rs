// Copyright 2018 foundationdb-rs developers, https://github.com/Clikengo/foundationdb-rs/graphs/contributors
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

use foundationdb::*;
use foundationdb_macros::cfg_api_versions;
use futures::future;
use futures::prelude::*;
use std::borrow::Cow;

mod common;

#[test]
fn test_range() {
    let _guard = unsafe { foundationdb::boot() };
    futures::executor::block_on(test_get_range_async()).expect("failed to run");
    futures::executor::block_on(test_range_option_async()).expect("failed to run");
    futures::executor::block_on(test_get_ranges_async()).expect("failed to run");
    #[cfg(any(feature = "fdb-6_3", feature = "fdb-7_0", feature = "fdb-7_1"))]
    {
        futures::executor::block_on(test_get_estimate_range()).expect("failed to run");
    }
    #[cfg(any(feature = "fdb-7_0", feature = "fdb-7_1"))]
    {
        futures::executor::block_on(test_get_range_split_points()).expect("failed to run");
    }
    #[cfg(any(feature = "fdb-7_1",))]
    {
        futures::executor::block_on(test_mapped_values()).expect("failed to run");
    }
}

async fn test_get_range_async() -> FdbResult<()> {
    const N: usize = 10000;

    let db = common::database().await?;

    {
        let trx = db.create_trx()?;
        let key_begin = "test-range-";
        let key_end = "test-range.";

        eprintln!("clearing...");
        trx.clear_range(key_begin.as_bytes(), key_end.as_bytes());

        eprintln!("inserting...");
        for _ in 0..N {
            let key = format!("{}-{}", key_begin, common::random_str(10));
            let value = common::random_str(10);
            trx.set(key.as_bytes(), value.as_bytes());
        }

        eprintln!("counting...");
        let begin = KeySelector::first_greater_or_equal(Cow::Borrowed(key_begin.as_bytes()));
        let end = KeySelector::first_greater_than(Cow::Borrowed(key_end.as_bytes()));
        let opt = RangeOption::from((begin, end));

        let range = trx.get_range(&opt, 1, false).await?;
        assert!(range.len() > 0);
        assert!(range.more());
        let len = range.len();
        let mut i = 0;
        for kv in &range {
            assert!(!kv.key().is_empty());
            assert!(!kv.value().is_empty());
            i += 1;
        }
        assert_eq!(i, len);

        let refs_asc = (&range).into_iter().collect::<Vec<_>>();
        assert_eq!(
            refs_asc,
            (&range).into_iter().rev().rev().collect::<Vec<_>>()
        );

        let owned_asc = trx
            .get_range(&opt, 1, false)
            .await?
            .into_iter()
            .collect::<Vec<_>>();
        assert_eq!(owned_asc, range.into_iter().rev().rev().collect::<Vec<_>>());
    }

    Ok(())
}

async fn test_get_ranges_async() -> FdbResult<()> {
    const N: usize = 10000;

    let db = common::database().await?;

    {
        let trx = db.create_trx()?;
        let key_begin = "test-ranges-";
        let key_end = "test-ranges.";

        eprintln!("clearing...");
        trx.clear_range(key_begin.as_bytes(), key_end.as_bytes());

        eprintln!("inserting...");
        for _ in 0..N {
            let key = format!("{}-{}", key_begin, common::random_str(10));
            let value = common::random_str(10);
            trx.set(key.as_bytes(), value.as_bytes());
        }

        eprintln!("counting...");
        let begin = KeySelector::first_greater_or_equal(Cow::Borrowed(key_begin.as_bytes()));
        let end = KeySelector::first_greater_than(Cow::Borrowed(key_end.as_bytes()));
        let opt = RangeOption::from((begin, end));

        let count = trx
            .get_ranges(opt, false)
            .try_fold(0usize, |count, kvs| future::ok(count + kvs.as_ref().len()))
            .await?;

        assert_eq!(count, N);
        eprintln!("count: {:?}", count);
    }

    Ok(())
}

async fn test_range_option_async() -> FdbResult<()> {
    let db = common::database().await?;

    {
        let trx = db.create_trx()?;
        let key_begin = "test-rangeoption-";
        let key_end = "test-rangeoption.";
        let k = |i: u32| format!("{}-{:010}", key_begin, i);

        eprintln!("clearing...");
        trx.clear_range(key_begin.as_bytes(), key_end.as_bytes());

        eprintln!("inserting...");
        for i in 0..10000 {
            let value = common::random_str(10);
            trx.set(k(i).as_bytes(), value.as_bytes());
        }
        assert_eq!(
            trx.get_ranges(
                (KeySelector::first_greater_or_equal(k(100).into_bytes())
                    ..KeySelector::first_greater_or_equal(k(5000).as_bytes()))
                    .into(),
                false
            )
            .try_fold(0usize, |count, kvs| future::ok(count + kvs.as_ref().len()))
            .await?,
            4900
        );
        assert_eq!(
            trx.get_ranges(
                (
                    KeySelector::first_greater_or_equal(k(100).into_bytes()),
                    KeySelector::first_greater_or_equal(k(5000).as_bytes())
                )
                    .into(),
                false
            )
            .try_fold(0usize, |count, kvs| future::ok(count + kvs.as_ref().len()))
            .await?,
            4900
        );
        assert_eq!(
            trx.get_ranges((k(100).into_bytes()..k(5000).into_bytes()).into(), false)
                .try_fold(0usize, |count, kvs| future::ok(count + kvs.as_ref().len()))
                .await?,
            4900
        );
        assert_eq!(
            trx.get_ranges((k(100).into_bytes(), k(5000).into_bytes()).into(), false)
                .try_fold(0usize, |count, kvs| future::ok(count + kvs.as_ref().len()))
                .await?,
            4900
        );
        assert_eq!(
            trx.get_ranges((k(100).as_bytes()..k(5000).as_bytes()).into(), false)
                .try_fold(0usize, |count, kvs| future::ok(count + kvs.as_ref().len()))
                .await?,
            4900
        );
        assert_eq!(
            trx.get_ranges((k(100).as_bytes(), k(5000).as_bytes()).into(), false)
                .try_fold(0usize, |count, kvs| future::ok(count + kvs.as_ref().len()))
                .await?,
            4900
        );

        assert_eq!(
            trx.get_ranges(
                (KeySelector::first_greater_or_equal(k(100).into_bytes())
                    ..KeySelector::first_greater_than(k(5000).as_bytes()))
                    .into(),
                false
            )
            .try_fold(0usize, |count, kvs| future::ok(count + kvs.as_ref().len()))
            .await?,
            4901
        );
        assert_eq!(
            trx.get_ranges((k(100).into_bytes()..=k(5000).into_bytes()).into(), false)
                .try_fold(0usize, |count, kvs| future::ok(count + kvs.as_ref().len()))
                .await?,
            4901
        );
        assert_eq!(
            trx.get_ranges((k(100).as_bytes()..=k(5000).as_bytes()).into(), false)
                .try_fold(0usize, |count, kvs| future::ok(count + kvs.as_ref().len()))
                .await?,
            4901
        );
    }

    Ok(())
}

#[cfg_api_versions(min = 630)]
async fn test_get_estimate_range() -> FdbResult<()> {
    const N: u32 = 10000;

    let db = common::database().await?;
    let trx = db.create_trx()?;
    let key_begin = "test-rangeoption-";
    let key_end = "test-rangeoption.";
    let k = |i: u32| format!("{}-{:010}", key_begin, i);

    eprintln!("clearing...");
    trx.clear_range(key_begin.as_bytes(), key_end.as_bytes());

    eprintln!("inserting...");
    for i in 0..N {
        let value = common::random_str(10);
        trx.set(k(i).as_bytes(), value.as_bytes());
    }
    trx.commit().await?;

    let trx = db.create_trx()?;
    let estimate = trx
        .get_estimated_range_size_bytes(key_begin.as_bytes(), key_end.as_bytes())
        .await?;
    eprintln!("get an estimate of {} bytes", estimate);
    assert!(estimate > 0);

    Ok(())
}

#[cfg_api_versions(min = 700)]
async fn test_get_range_split_points() -> FdbResult<()> {
    const N: u32 = 10000;

    let db = common::database().await?;
    let trx = db.create_trx()?;
    let key_begin = "test-split-point-";
    let key_end = "test-split-point.";
    let k = |i: u32| format!("{}-{:010}", key_begin, i);

    eprintln!("clearing...");
    trx.clear_range(key_begin.as_bytes(), key_end.as_bytes());

    eprintln!("inserting...");
    for i in 0..N {
        let value = common::random_str(10);
        trx.set(k(i).as_bytes(), value.as_bytes());
    }
    trx.commit().await?;

    let trx = db.create_trx()?;
    let splits = trx
        .get_range_split_points(key_begin.as_bytes(), key_end.as_bytes(), 100)
        .await?;
    assert!(splits.len() > 0);
    for split in splits {
        eprintln!("split point: {:?}", split.key());
    }
    Ok(())
}

#[cfg_api_versions(min = 710)]
async fn test_mapped_values() -> FdbResult<()> {
    use foundationdb::tuple::{pack, unpack, Element, Subspace};
    use std::borrow::Cow;

    let db = common::database().await?;

    let data_subspace = Subspace::all().subspace(&("data"));
    let index_subspace = Subspace::all().subspace(&("index"));
    let number_of_records: i32 = 20;

    // setup
    let setup_transaction = db.create_trx()?;
    let mut blue_counter = 0;
    for primary_key in 0_i32..number_of_records {
        let eye_color = match primary_key % 3 {
            0 => {
                blue_counter += 1;
                "blue"
            }
            1 => "brown",
            2 => "green",
            _ => unreachable!(),
        };

        // write into the data subspace
        setup_transaction.set(
            &data_subspace.pack(&(primary_key, "eye_color", eye_color)),
            eye_color.as_bytes(),
        );
        // write another key next to it
        setup_transaction.set(
            &data_subspace.pack(&(primary_key, "some_data")),
            &pack(&("fdb-rs")),
        );
        // write into the index subspace
        setup_transaction.set(&index_subspace.pack(&(eye_color, primary_key)), &[]);
    }
    setup_transaction.commit().await.expect("could not commit");

    let t = db.create_trx()?;
    let range_option = RangeOption::from(&index_subspace.subspace(&("blue")));

    // The mapper is a Tuple that allow to transform keys.
    // This one is allowing fdb to convert a key like `("index", "blue", PRIMARY_KEY)`
    // to generate a scan in the range `("data", PRIMARY_KEY, ...)`
    // More info can be found here: https://github.com/apple/foundationdb/wiki/Everything-about-GetMappedRange
    let mapper = pack(&("data", "{K[2]}", "{...}"));

    let mapped_key_values = t
        .get_mapped_range(&range_option, &mapper, 1024, false)
        .await?;

    assert_eq!(
        mapped_key_values.len() as i32,
        blue_counter,
        "found {} elements instead of {}",
        mapped_key_values.len(),
        blue_counter
    );

    for mapped_key_value in mapped_key_values {
        // checking the parent key that generated the scan
        let parent_key: Vec<Element> =
            unpack(mapped_key_value.parent_key()).expect("could not unpack index key");
        assert!(parent_key.starts_with(&[
            Element::String(Cow::from("index")),
            Element::String(Cow::from("blue"))
        ]));

        let mapped_values = mapped_key_value.key_values();
        assert_eq!(
            mapped_values.len(),
            2,
            "bad length, expecting 2, got {}",
            mapped_values.len()
        );

        for kv in mapped_values {
            let key: Vec<Element> = unpack(kv.key()).expect("could not unpack key");
            assert!(key.starts_with(&[Element::String(Cow::from("data"))]))
        }
    }

    Ok(())
}
