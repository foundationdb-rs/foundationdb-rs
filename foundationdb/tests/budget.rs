// Copyright 2018 foundationdb-rs developers, https://github.com/Clikengo/foundationdb-rs/graphs/contributors
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Per-attempt usage accounting and client-side budget, against a live cluster.

use foundationdb::options::StreamingMode;
use foundationdb::*;
use std::time::Duration;

mod common;

/// `transaction_too_old`, retryable, retried without any backoff.
const TRANSACTION_TOO_OLD: i32 = 1007;

/// Writes are accounted as soon as they are issued, no commit needed.
#[tokio::test]
async fn usage_counts_writes() -> FdbResult<()> {
    const KEY: &[u8] = b"test-budget-writes-key";
    const VALUE: &[u8] = b"value";
    const PARAM: &[u8] = b"\x01\x00\x00\x00\x00\x00\x00\x00";
    const BEGIN: &[u8] = b"test-budget-writes-";
    const END: &[u8] = b"test-budget-writes.";

    let db = common::database().await?;
    let trx = db.create_trx()?;

    trx.set(KEY, VALUE);
    trx.clear(KEY);
    trx.clear_range(BEGIN, END);
    trx.atomic_op(KEY, PARAM, options::MutationType::Add);

    let usage = trx.attempt_usage();
    assert_eq!(usage.call_set, 1);
    assert_eq!(usage.call_clear, 1);
    assert_eq!(usage.call_clear_range, 1);
    assert_eq!(usage.call_atomic_op, 1);
    assert_eq!(
        usage.bytes_written,
        (KEY.len() + VALUE.len() + KEY.len() + BEGIN.len() + END.len() + KEY.len() + PARAM.len())
            as u64
    );
    assert_eq!(usage.bytes_read, 0);

    Ok(())
}

/// Reads are accounted when their future resolves.
#[tokio::test]
async fn usage_counts_reads() -> FdbResult<()> {
    const PREFIX: &[u8] = b"test-budget-reads-";
    const MISSING: &[u8] = b"test-budget-reads-missing-key";
    const VALUE: &[u8] = b"value";

    let keys: Vec<Vec<u8>> = (0..3)
        .map(|i| [PREFIX, format!("{i}").as_bytes()].concat())
        .collect();

    let db = common::database().await?;

    {
        let trx = db.create_trx()?;
        trx.clear_range(PREFIX, b"test-budget-reads.");
        for key in &keys {
            trx.set(key, VALUE);
        }
        trx.commit().await.expect("failed to commit");
    }

    // get, on an existing then on a missing key.
    let trx = db.create_trx()?;
    trx.get(&keys[0], false).await?;
    trx.get(MISSING, false).await?;

    let usage = trx.attempt_usage();
    assert_eq!(usage.call_get, 2);
    assert_eq!(usage.keys_values_fetched, 1);
    assert_eq!(
        usage.bytes_read,
        (keys[0].len() + VALUE.len() + MISSING.len()) as u64
    );

    // get_key counts as a get of the selector key plus the resolved key.
    let trx = db.create_trx()?;
    let selector = KeySelector::first_greater_or_equal(keys[0].as_slice());
    let resolved = trx.get_key(&selector, false).await?;

    let usage = trx.attempt_usage();
    assert_eq!(usage.call_get, 1);
    assert_eq!(usage.keys_values_fetched, 0);
    assert_eq!(usage.bytes_read, (keys[0].len() + resolved.len()) as u64);

    // get_range counts one call per resolved batch.
    let trx = db.create_trx()?;
    let opt = RangeOption {
        mode: StreamingMode::WantAll,
        ..RangeOption::from((PREFIX, b"test-budget-reads.".as_ref()))
    };
    let values = trx.get_range(&opt, 1, false).await?;
    assert_eq!(values.len(), keys.len());

    let usage = trx.attempt_usage();
    assert_eq!(usage.call_get_range, 1);
    assert_eq!(usage.call_get, 0);
    assert_eq!(usage.keys_values_fetched, keys.len() as u64);
    assert_eq!(
        usage.bytes_read,
        keys.iter().map(|k| (k.len() + VALUE.len()) as u64).sum()
    );

    Ok(())
}

#[tokio::test]
async fn budget_exceeded_on_bytes_written() -> FdbResult<()> {
    const KEY: &[u8] = b"test-budget-bytes-written-key";
    const VALUE: &[u8] = b"a-value-larger-than-the-limit";

    let db = common::database().await?;
    let trx = db.create_trx()?;

    trx.set_client_budget(ClientBudget {
        max_bytes_written: Some(8),
        ..ClientBudget::default()
    });
    assert!(trx.check_client_budget().is_ok());

    trx.set(KEY, VALUE);

    let err = trx.check_client_budget().unwrap_err();
    assert_eq!(err.kind, BudgetKind::BytesWritten);
    assert_eq!(err.used, (KEY.len() + VALUE.len()) as u64);
    assert_eq!(err.limit, 8);

    Ok(())
}

#[tokio::test]
async fn budget_exceeded_on_bytes_read() -> FdbResult<()> {
    const KEY: &[u8] = b"test-budget-bytes-read-key";
    const VALUE: &[u8] = b"a-value-larger-than-the-limit";

    let db = common::database().await?;

    {
        let trx = db.create_trx()?;
        trx.set(KEY, VALUE);
        trx.commit().await.expect("failed to commit");
    }

    let trx = db.create_trx()?;
    trx.set_client_budget(ClientBudget {
        max_bytes_read: Some(8),
        ..ClientBudget::default()
    });
    assert!(trx.check_client_budget().is_ok());

    trx.get(KEY, false).await?;

    let err = trx.check_client_budget().unwrap_err();
    assert_eq!(err.kind, BudgetKind::BytesRead);
    assert_eq!(err.used, (KEY.len() + VALUE.len()) as u64);
    assert_eq!(err.limit, 8);

    Ok(())
}

#[tokio::test]
async fn budget_exceeded_on_time() -> FdbResult<()> {
    let db = common::database().await?;
    let trx = db.create_trx()?;

    trx.set_client_budget(ClientBudget {
        time_limit: Some(Duration::from_millis(20)),
        ..ClientBudget::default()
    });
    assert!(trx.check_client_budget().is_ok());

    tokio::time::sleep(Duration::from_millis(50)).await;

    let err = trx.check_client_budget().unwrap_err();
    assert_eq!(err.kind, BudgetKind::Time);
    assert!(err.used >= 20, "used {} ms", err.used);
    assert_eq!(err.limit, 20);

    Ok(())
}

/// The budget is configuration, the usage is per-attempt: `on_error` keeps the
/// former and resets the latter.
#[tokio::test]
async fn budget_survives_on_error_while_usage_resets() -> FdbResult<()> {
    const KEY: &[u8] = b"test-budget-on-error-key";
    const VALUE: &[u8] = b"a-value-larger-than-the-limit";

    let db = common::database().await?;
    let trx = db.create_trx()?;

    trx.set_client_budget(ClientBudget {
        max_bytes_written: Some(8),
        ..ClientBudget::default()
    });
    trx.set(KEY, VALUE);
    assert!(trx.check_client_budget().is_err());

    let trx = trx
        .on_error(FdbError::from_code(TRANSACTION_TOO_OLD))
        .await?;

    let usage = trx.attempt_usage();
    assert_eq!(usage.bytes_written, 0);
    assert_eq!(usage.call_set, 0);
    assert!(
        trx.check_client_budget().is_ok(),
        "usage should have been reset by the new attempt"
    );

    // The limit is still armed for the new attempt.
    trx.set(KEY, VALUE);
    assert_eq!(
        trx.check_client_budget().unwrap_err().kind,
        BudgetKind::BytesWritten
    );

    Ok(())
}

/// Same contract for an explicit `reset`.
#[tokio::test]
async fn reset_starts_a_new_attempt() -> FdbResult<()> {
    const KEY: &[u8] = b"test-budget-reset-key";
    const VALUE: &[u8] = b"a-value-larger-than-the-limit";

    let db = common::database().await?;
    let mut trx = db.create_trx()?;

    trx.set_client_budget(ClientBudget {
        max_bytes_written: Some(8),
        ..ClientBudget::default()
    });
    trx.set(KEY, VALUE);
    assert!(trx.check_client_budget().is_err());

    trx.reset();

    assert_eq!(trx.attempt_usage().bytes_written, 0);
    assert!(trx.check_client_budget().is_ok());

    trx.clear_client_budget();
    trx.set(KEY, VALUE);
    assert!(trx.check_client_budget().is_ok());

    Ok(())
}

/// An exceeded budget converts into an `FdbBindingError`, so `?` works inside a
/// `run` closure, and it is not retried.
#[tokio::test]
async fn budget_exceeded_is_fatal_in_run() -> FdbResult<()> {
    const KEY: &[u8] = b"test-budget-run-key";
    const VALUE: &[u8] = b"a-value-larger-than-the-limit";

    let db = common::database().await?;

    let result: Result<(), FdbBindingError> = db
        .run(|trx, _| async move {
            trx.set_client_budget(ClientBudget {
                max_bytes_written: Some(8),
                ..ClientBudget::default()
            });
            trx.set(KEY, VALUE);
            trx.check_client_budget()?;
            Ok(())
        })
        .await;

    match result {
        Err(FdbBindingError::ClientBudgetExceeded(err)) => {
            assert_eq!(err.kind, BudgetKind::BytesWritten);
            assert!(err.to_string().contains("client-side estimate"));
        }
        other => panic!("expected a ClientBudgetExceeded error, got {other:?}"),
    }

    Ok(())
}
