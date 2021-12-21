// Copyright 2018 foundationdb-rs developers, https://github.com/Clikengo/foundationdb-rs/graphs/contributors
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

use foundationdb::*;
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
    #[cfg(feature = "fdb-6_3")]
    futures::executor::block_on(test_get_estimate_range()).expect("failed to run");
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
        assert_eq!(range.more(), true);
        let len = range.len();
        let mut i = 0;
        for kv in &range {
            assert!(kv.key().len() > 0);
            assert!(kv.value().len() > 0);
            i += 1;
        }
        assert_eq!(i, len);

        let refs_asc = (&range).into_iter().collect::<Vec<_>>();
        let refs_desc = (&range).into_iter().rev().collect::<Vec<_>>();
        assert_eq!(refs_asc, refs_desc.into_iter().rev().collect::<Vec<_>>());

        let owned_asc = trx
            .get_range(&opt, 1, false)
            .await?
            .into_iter()
            .collect::<Vec<_>>();
        let owned_desc = range.into_iter().rev().collect::<Vec<_>>();
        assert_eq!(owned_asc, owned_desc.into_iter().rev().collect::<Vec<_>>());
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
async fn test_get_estimate_range() -> FdbResult<()> {
    const N: usize = 10000;

    let db = common::database().await?;
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
    trx.commit().await?;

    let trx = db.create_trx()?;
    let estimate = trx
        .get_estimated_range_size_bytes(key_begin.as_bytes(), key_end.as_bytes())
        .await?;
    eprintln!("get an estimate of {} bytes", estimate);
    assert!(estimate > 0);

    Ok(())
}
