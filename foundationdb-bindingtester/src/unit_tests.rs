//! The `UNIT_TESTS` instruction: a port of the Python bindings unit tests
//! (`bindings/python/tests/unit_tests.py`, `cancellation_timeout_tests.py` and
//! `size_limit_tests.py`).
//!
//! A failed check panics, which makes the harness report the tester as failed, like the
//! exception the Python tester raises. Unexpected FDB errors go back to the retry loop of
//! each test, as in Python.
//!
//! Waiting never uses the wall clock: [`delay`] waits on a transaction timeout, so the
//! time comes from the FDB client.

use std::future::Future;
use std::sync::atomic::{AtomicI32, Ordering};

use foundationdb::options::{ConflictRangeType, DatabaseOption, MutationType, TransactionOption};
use foundationdb::{
    Database, FdbError, FdbResult, Transaction, TransactionCancelled, TransactionCommitError,
};
use futures::FutureExt;
use futures::future::BoxFuture;

/// The API version the tester runs at, set by `main`.
pub static API_VERSION: AtomicI32 = AtomicI32::new(foundationdb_sys::FDB_API_VERSION as i32);

const DEFAULT_TIMEOUT_MS: i64 = 60_000;

const TRANSACTION_TOO_OLD: i32 = 1007;
const FUTURE_VERSION: i32 = 1009;
const NOT_COMMITTED: i32 = 1020;
const TRANSACTION_CANCELLED: i32 = 1025;
const TRANSACTION_TIMED_OUT: i32 = 1031;
const TRANSACTION_TOO_LARGE: i32 = 2101;

fn api_version() -> i32 {
    API_VERSION.load(Ordering::Relaxed)
}

fn error(code: i32) -> FdbError {
    FdbError::from_code(code)
}

/// Accepts error `code`, fails the unit tests if `result` succeeded, and hands any other
/// error back to the retry loop.
fn expect_error<T>(result: FdbResult<T>, code: i32, what: &str) -> FdbResult<()> {
    match result {
        Ok(_) => panic!("unit test failed: {what}"),
        Err(e) if e.code() == code => Ok(()),
        Err(e) => Err(e),
    }
}

/// Like [`expect_error`], for a commit, and gives the failed transaction back.
fn expect_commit_error(
    result: Result<impl Sized, TransactionCommitError>,
    code: i32,
    what: &str,
) -> FdbResult<TransactionCommitError> {
    match result {
        Ok(_) => panic!("unit test failed: {what}"),
        Err(e) if e.code() == code => Ok(e),
        Err(e) => Err(e.into()),
    }
}

async fn commit(tr: Transaction) -> FdbResult<()> {
    tr.commit().await.map(|_| ()).map_err(FdbError::from)
}

/// Calls `on_error` and keeps the transaction, whether the error was retried or not.
async fn on_error(tr: Transaction, code: i32) -> (Transaction, FdbResult<()>) {
    tr.on_error_with_transaction(error(code)).await
}

/// Gives a cancelled transaction back as a [`Transaction`], to check that the native client
/// rejects every use of it. The bindingtester does the same for its `CANCEL` instruction.
fn cancelled(tr: TransactionCancelled) -> Transaction {
    // SAFETY: `TransactionCancelled` only wraps the `Transaction`, see `main.rs`.
    unsafe { std::mem::transmute::<TransactionCancelled, Transaction>(tr) }
}

/// Waits `ms` milliseconds, as measured by the FDB client: a fresh transaction with that
/// timeout waits for the versionstamp of a commit that never happens, until the timeout
/// fails it. The time starts when this function is called, not when the future is first
/// polled.
fn delay(db: &Database, ms: i64) -> FdbResult<impl Future<Output = ()> + use<>> {
    let tr = db.create_trx()?;
    tr.set_option(TransactionOption::Timeout(ms))?;
    let versionstamp = tr.get_versionstamp();
    Ok(async move {
        let result = versionstamp.await;
        drop(tr);
        assert_eq!(
            result.map(|_| ()).map_err(|e| e.code()),
            Err(TRANSACTION_TIMED_OUT),
            "unit test failed: the delay transaction did not time out"
        );
    })
}

async fn sleep(db: &Database, ms: i64) -> FdbResult<()> {
    delay(db, ms)?.await;
    Ok(())
}

/// Runs `f` on fresh transactions until it succeeds, giving up after `DEFAULT_TIMEOUT_MS`.
/// The Python `retry_with_timeout` decorator.
async fn retry_with_timeout<T>(
    db: &Database,
    mut f: impl AsyncFnMut(Transaction) -> FdbResult<T>,
) -> FdbResult<T> {
    let mut timeout_tr = db.create_trx()?;
    loop {
        timeout_tr.set_option(TransactionOption::Timeout(DEFAULT_TIMEOUT_MS))?;
        match f(db.create_trx()?).await {
            Ok(value) => return Ok(value),
            Err(e) => timeout_tr = timeout_tr.on_error(e).await?,
        }
    }
}

/// Applies `f` and commits, with retries. The Python `@fdb.transactional` decorator.
async fn transact(db: &Database, f: impl Fn(&Transaction)) -> FdbResult<()> {
    retry_with_timeout(db, async |tr| {
        f(&tr);
        commit(tr).await
    })
    .await
}

/// Runs every unit test, in the order of the Python tester.
pub async fn run_unit_tests(db: &Database) -> FdbResult<()> {
    log::debug!("unit test db_options");
    test_db_options(db)?;
    log::debug!("unit test options");
    test_options(db).await?;
    log::debug!("unit test watches");
    test_watches(db).await?;
    log::debug!("unit test cancellation");
    test_cancellation(db).await?;
    log::debug!("unit test retry_limits");
    test_retry_limits(db).await?;
    log::debug!("unit test db_retry_limits");
    test_db_retry_limits(db).await?;
    log::debug!("unit test timeouts");
    test_timeouts(db).await?;
    log::debug!("unit test db_timeouts");
    test_db_timeouts(db).await?;
    log::debug!("unit test combinations");
    test_combinations(db).await?;
    // test_locality needs get_boundary_keys, see #396.
    test_predicates();
    log::debug!("unit test size_limit_option");
    test_size_limit_option(db).await?;
    if api_version() >= 620 {
        log::debug!("unit test get_approximate_size");
        test_get_approximate_size(db).await?;
    }
    if api_version() >= 730 {
        log::debug!("unit test get_client_status");
        test_get_client_status(db).await?;
    }
    if api_version() >= 700 {
        log::debug!("unit test range_split_points");
        test_range_split_points(db).await?;
    }
    Ok(())
}

fn test_db_options(db: &Database) -> FdbResult<()> {
    db.set_option(DatabaseOption::LocationCacheSize(100_001))?;
    db.set_option(DatabaseOption::MaxWatches(100_001))?;
    db.set_option(DatabaseOption::DatacenterId("dc_id".to_string()))?;
    db.set_option(DatabaseOption::MachineId("machine_id".to_string()))?;
    db.set_option(DatabaseOption::SnapshotRywEnable)?;
    db.set_option(DatabaseOption::SnapshotRywDisable)?;
    db.set_option(DatabaseOption::TransactionLoggingMaxFieldLength(1000))?;
    db.set_option(DatabaseOption::TransactionTimeout(100_000))?;
    db.set_option(DatabaseOption::TransactionTimeout(0))?;
    db.set_option(DatabaseOption::TransactionTimeout(0))?;
    db.set_option(DatabaseOption::TransactionMaxRetryDelay(100))?;
    db.set_option(DatabaseOption::TransactionSizeLimit(100_000))?;
    db.set_option(DatabaseOption::TransactionRetryLimit(10))?;
    db.set_option(DatabaseOption::TransactionRetryLimit(-1))?;
    db.set_option(DatabaseOption::TransactionCausalReadRisky)?;
    db.set_option(DatabaseOption::TransactionIncludePortInAddress)?;
    Ok(())
}

async fn test_options(db: &Database) -> FdbResult<()> {
    retry_with_timeout(db, async |tr| {
        tr.set_option(TransactionOption::PrioritySystemImmediate)?;
        tr.set_option(TransactionOption::PriorityBatch)?;
        tr.set_option(TransactionOption::CausalReadRisky)?;
        tr.set_option(TransactionOption::CausalWriteRisky)?;
        tr.set_option(TransactionOption::ReadYourWritesDisable)?;
        tr.set_option(TransactionOption::ReadSystemKeys)?;
        tr.set_option(TransactionOption::AccessSystemKeys)?;
        tr.set_option(TransactionOption::TransactionLoggingMaxFieldLength(1000))?;
        tr.set_option(TransactionOption::Timeout(60 * 1000))?;
        tr.set_option(TransactionOption::RetryLimit(50))?;
        tr.set_option(TransactionOption::MaxRetryDelay(100))?;
        tr.set_option(TransactionOption::UsedDuringCommitProtectionDisable)?;
        tr.set_option(TransactionOption::DebugTransactionIdentifier(
            "my_transaction".to_string(),
        ))?;
        tr.set_option(TransactionOption::LogTransaction)?;
        tr.set_option(TransactionOption::ReadLockAware)?;
        tr.set_option(TransactionOption::LockAware)?;
        tr.set_option(TransactionOption::IncludePortInAddress)?;
        tr.get(b"\xff", false).await?;
        commit(tr).await
    })
    .await
}

type Watch = BoxFuture<'static, FdbResult<()>>;

/// Returns false, after `on_error`, when a watch failed and the test must start over.
async fn check_watches(db: &Database, watches: &mut [Watch], expected: bool) -> FdbResult<bool> {
    for (i, watch) in watches.iter_mut().enumerate() {
        let result = if expected {
            Some(watch.await)
        } else {
            watch.now_or_never()
        };
        match result {
            None => {}
            Some(Ok(())) => assert!(expected, "unit test failed: watch {i} is ready"),
            Some(Err(e)) => {
                db.create_trx()?.on_error(e).await?;
                return Ok(false);
            }
        }
    }
    Ok(true)
}

async fn test_watches(db: &Database) -> FdbResult<()> {
    loop {
        transact(db, |tr| {
            tr.set(b"w0", b"0");
            tr.set(b"w3", b"3");
        })
        .await?;

        let mut watches: Vec<Watch> = Vec::new();
        watches.push(
            retry_with_timeout(db, async |tr| {
                let mut watch = tr.watch(b"w0").boxed();
                tr.set(b"w0", b"0");
                assert!(
                    (&mut watch).now_or_never().is_none(),
                    "unit test failed: watch 0 is ready before commit"
                );
                commit(tr).await?;
                Ok(watch)
            })
            .await?,
        );
        // clear_and_watch, set_and_watch and get_and_watch.
        watches.push(
            retry_with_timeout(db, async |tr| {
                tr.clear(b"w1");
                let watch = tr.watch(b"w1").boxed();
                commit(tr).await?;
                Ok(watch)
            })
            .await?,
        );
        watches.push(
            retry_with_timeout(db, async |tr| {
                tr.set(b"w2", b"2");
                let watch = tr.watch(b"w2").boxed();
                commit(tr).await?;
                Ok(watch)
            })
            .await?,
        );
        let (value, watch) = retry_with_timeout(db, async |tr| {
            let value = tr.get(b"w3", false).await?.map(|v| v.to_vec());
            let watch = tr.watch(b"w3").boxed();
            commit(tr).await?;
            Ok((value, watch))
        })
        .await?;
        assert_eq!(value.as_deref(), Some(b"3".as_slice()));
        watches.push(watch);

        sleep(db, 1000).await?;
        if !check_watches(db, &mut watches, false).await? {
            continue;
        }

        transact(db, |tr| tr.clear(b"w1")).await?;
        sleep(db, 5000).await?;
        if !check_watches(db, &mut watches, false).await? {
            continue;
        }

        transact(db, |tr| {
            tr.set(b"w0", b"a");
            tr.set(b"w1", b"b");
            tr.clear(b"w2");
            tr.atomic_op(b"w3", b"\xff\xff", MutationType::BitXor);
        })
        .await?;
        if check_watches(db, &mut watches, true).await? {
            return Ok(());
        }
    }
}

async fn test_cancellation(db: &Database) -> FdbResult<()> {
    // (1) Basic cancellation
    retry_with_timeout(db, async |tr| {
        let tr = cancelled(tr.cancel());
        expect_error(
            commit(tr).await,
            TRANSACTION_CANCELLED,
            "basic cancellation",
        )
    })
    .await?;

    // (2) Cancellation does not survive reset
    retry_with_timeout(db, async |tr| {
        let tr = tr.cancel().reset();
        match commit(tr).await {
            Err(e) if e.code() == TRANSACTION_CANCELLED => {
                panic!("unit test failed: cancellation survived reset")
            }
            r => r,
        }
    })
    .await?;

    // (3) Cancellation does survive on_error()
    retry_with_timeout(db, async |tr| {
        let tr = cancelled(tr.cancel());
        let (tr, r) = on_error(tr, TRANSACTION_TOO_OLD).await;
        expect_error(
            r,
            TRANSACTION_CANCELLED,
            "on_error() did not notice cancellation",
        )?;
        expect_error(
            commit(tr).await,
            TRANSACTION_CANCELLED,
            "cancellation did not survive on_error()",
        )
    })
    .await?;

    // (4) Cancellation works with weird operations
    retry_with_timeout(db, async |tr| {
        // A read in flight when the transaction is cancelled, not awaited, as in Python.
        drop(tr.get(b"foo", false));
        let tr = cancelled(tr.cancel());
        expect_error(
            tr.get_read_version().await,
            TRANSACTION_CANCELLED,
            "cancellation didn't throw on weird operation",
        )
    })
    .await
}

async fn test_retry_limits(db: &Database) -> FdbResult<()> {
    let err = TRANSACTION_TOO_OLD;

    // (1) Basic retry limits
    retry_with_timeout(db, async |tr| {
        tr.set_option(TransactionOption::RetryLimit(1))?;
        tr.set(b"foo", b"bar");
        let (tr, r) = on_error(tr, err).await;
        r?;
        tr.set(b"foo", b"bar");
        tr.set_option(TransactionOption::RetryLimit(1))?;
        let (tr, r) = on_error(tr, err).await;
        expect_error(r, err, "(1) retry limit was ignored")?;
        tr.set(b"foo", b"bar");
        tr.set_option(TransactionOption::RetryLimit(1))?;
        let (_, r) = on_error(tr, err).await;
        expect_error(
            r,
            TRANSACTION_CANCELLED,
            "(1) transaction not cancelled after error",
        )
    })
    .await?;

    // (2) Retry limits don't survive resets
    retry_with_timeout(db, async |tr| {
        tr.set_option(TransactionOption::RetryLimit(1))?;
        tr.set(b"foo", b"bar");
        let (tr, r) = on_error(tr, err).await;
        r?;
        tr.set_option(TransactionOption::RetryLimit(1))?;
        tr.set(b"foo", b"bar");
        let (mut tr, r) = on_error(tr, err).await;
        expect_error(r, err, "(2) retry limit was ignored")?;
        tr.reset();
        tr.set(b"foo", b"bar");
        on_error(tr, err).await.1
    })
    .await?;

    // (3) Number of retries does not survive resets
    retry_with_timeout(db, async |tr| {
        tr.set_option(TransactionOption::RetryLimit(0))?;
        tr.set(b"foo", b"bar");
        let (mut tr, r) = on_error(tr, err).await;
        expect_error(r, err, "(3) retry limit was ignored")?;
        tr.reset();
        tr.set_option(TransactionOption::RetryLimit(1))?;
        tr.set(b"foo", b"bar");
        on_error(tr, err).await.1
    })
    .await?;

    // (4) Retries accumulate when limits are turned off, and are respected retroactively
    retry_with_timeout(db, async |tr| {
        tr.set(b"foo", b"bar");
        let (tr, r) = on_error(tr, err).await;
        r?;
        tr.set(b"foo", b"bar");
        let (tr, r) = on_error(tr, err).await;
        r?;
        tr.set_option(TransactionOption::RetryLimit(1))?;
        let (mut tr, r) = on_error(tr, err).await;
        expect_error(r, err, "(4) retry limit was ignored")?;
        tr.reset();
        let mut tr = tr;
        for limit in 1..=4 {
            tr.set(b"foo", b"bar");
            tr.set_option(TransactionOption::RetryLimit(limit))?;
            let (next, r) = on_error(tr, err).await;
            r?;
            tr = next;
        }
        tr.set_option(TransactionOption::RetryLimit(4))?;
        let (tr, r) = on_error(tr, err).await;
        expect_error(r, err, "(4) retry limit was ignored")?;
        tr.set_option(TransactionOption::RetryLimit(4))?;
        let (_, r) = on_error(tr, err).await;
        expect_error(
            r,
            TRANSACTION_CANCELLED,
            "(4) transaction not cancelled after error",
        )
    })
    .await?;

    // (5) Retry limits survive on_error()
    retry_with_timeout(db, async |tr| {
        tr.set_option(TransactionOption::RetryLimit(2))?;
        tr.set(b"foo", b"bar");
        let (tr, r) = on_error(tr, err).await;
        r?;
        tr.set(b"foo", b"bar");
        let (tr, r) = on_error(tr, err).await;
        r?;
        let (tr, r) = on_error(tr, err).await;
        expect_error(r, err, "(5) retry limit was ignored")?;
        tr.set(b"foo", b"bar");
        let (_, r) = on_error(tr, err).await;
        expect_error(
            r,
            TRANSACTION_CANCELLED,
            "(5) transaction not cancelled after error",
        )
    })
    .await
}

async fn test_db_retry_limits(db: &Database) -> FdbResult<()> {
    let err = TRANSACTION_TOO_OLD;
    db.set_option(DatabaseOption::TransactionRetryLimit(1))?;

    // (1) Basic retry limit
    retry_with_timeout(db, async |tr| {
        tr.set(b"foo", b"bar");
        let (tr, r) = on_error(tr, err).await;
        r?;
        tr.set(b"foo", b"bar");
        let (tr, r) = on_error(tr, err).await;
        expect_error(r, err, "(1) retry limit from database was ignored")?;
        tr.set(b"foo", b"bar");
        let (_, r) = on_error(tr, err).await;
        expect_error(
            r,
            TRANSACTION_CANCELLED,
            "(1) transaction not cancelled after error",
        )
    })
    .await?;

    // (2) More retries in transaction than database
    retry_with_timeout(db, async |tr| {
        tr.set_option(TransactionOption::RetryLimit(2))?;
        tr.set(b"foo", b"bar");
        let (tr, r) = on_error(tr, err).await;
        r?;
        tr.set(b"foo", b"bar");
        let (tr, r) = on_error(tr, err).await;
        r?;
        tr.set(b"foo", b"bar");
        let (tr, r) = on_error(tr, err).await;
        expect_error(r, err, "(2) retry limit from transaction was ignored")?;
        tr.set(b"foo", b"bar");
        let (_, r) = on_error(tr, err).await;
        expect_error(
            r,
            TRANSACTION_CANCELLED,
            "(2) transaction not cancelled after error",
        )
    })
    .await?;

    // (3) More retries in database than transaction
    db.set_option(DatabaseOption::TransactionRetryLimit(2))?;
    retry_with_timeout(db, async |tr| {
        tr.set_option(TransactionOption::RetryLimit(1))?;
        tr.set(b"foo", b"bar");
        let (tr, r) = on_error(tr, err).await;
        r?;
        tr.set(b"foo", b"bar");
        let (tr, r) = on_error(tr, err).await;
        expect_error(r, err, "(3) retry limit from transaction was ignored")?;
        tr.set(b"foo", b"bar");
        let (_, r) = on_error(tr, err).await;
        expect_error(
            r,
            TRANSACTION_CANCELLED,
            "(3) transaction not cancelled after error",
        )
    })
    .await?;

    // (4) Reset resets to database default
    retry_with_timeout(db, async |tr| {
        tr.set_option(TransactionOption::RetryLimit(1))?;
        tr.set(b"foo", b"bar");
        let (tr, r) = on_error(tr, err).await;
        r?;
        tr.set(b"foo", b"bar");
        let (mut tr, r) = on_error(tr, err).await;
        expect_error(r, err, "(4) retry limit from transaction was ignored")?;
        tr.reset();
        tr.set(b"foo", b"bar");
        let (tr, r) = on_error(tr, err).await;
        r?;
        tr.set(b"foo", b"bar");
        let (tr, r) = on_error(tr, err).await;
        r?;
        tr.set(b"foo", b"bar");
        let (tr, r) = on_error(tr, err).await;
        expect_error(r, err, "(4) retry limit from database was ignored")?;
        let (_, r) = on_error(tr, err).await;
        expect_error(
            r,
            TRANSACTION_CANCELLED,
            "(4) transaction not cancelled after error",
        )
    })
    .await?;

    // Back to the default, infinite retries.
    db.set_option(DatabaseOption::TransactionRetryLimit(-1))
}

async fn test_timeouts(db: &Database) -> FdbResult<()> {
    let timed_out = TRANSACTION_TIMED_OUT;

    // (1) Basic timeouts
    retry_with_timeout(db, async |tr| {
        tr.set_option(TransactionOption::Timeout(10))?;
        sleep(db, 1000).await?;
        expect_error(commit(tr).await, timed_out, "timeout didn't fire")
    })
    .await?;

    // (2) Timeout survives on_error()
    retry_with_timeout(db, async |tr| {
        tr.set_option(TransactionOption::Timeout(100))?;
        let (tr, r) = on_error(tr, TRANSACTION_TOO_OLD).await;
        r?;
        sleep(db, 1000).await?;
        match commit(tr).await {
            Err(e) if e.code() != timed_out => Err(e),
            _ => Ok(()),
        }
    })
    .await?;

    // (3) Timeout having fired survives on_error()
    retry_with_timeout(db, async |tr| {
        tr.set_option(TransactionOption::Timeout(100))?;
        sleep(db, 1000).await?;
        let (tr, r) = on_error(tr, TRANSACTION_TOO_OLD).await;
        expect_error(r, timed_out, "timeout didn't fire")?;
        expect_error(commit(tr).await, timed_out, "timeout didn't fire")
    })
    .await?;

    // (4) Timeout does not survive reset
    retry_with_timeout(db, async |mut tr| {
        tr.set_option(TransactionOption::Timeout(100))?;
        tr.reset();
        sleep(db, 1000).await?;
        commit(tr).await
    })
    .await?;

    // (5) Timeout having fired does not survive reset
    retry_with_timeout(db, async |tr| {
        tr.set_option(TransactionOption::Timeout(100))?;
        sleep(db, 1000).await?;
        let tr = match tr.commit().await {
            Ok(c) => c.reset(),
            Err(e) if e.code() == timed_out => e.reset(),
            Err(e) => return Err(e.into()),
        };
        commit(tr).await
    })
    .await?;

    // (6) Timeout will fire "retroactively"
    retry_with_timeout(db, async |tr| {
        tr.set(b"foo", b"bar");
        sleep(db, 1000).await?;
        tr.set_option(TransactionOption::Timeout(10))?;
        expect_error(commit(tr).await, timed_out, "timeout didn't fire")
    })
    .await?;

    // (7) Transaction reset also resets time from which timeout is measured
    retry_with_timeout(db, async |mut tr| {
        tr.set(b"foo", b"bar");
        sleep(db, 1000).await?;
        let mut min_elapsed = delay(db, 490)?.boxed();
        tr.reset();
        tr.set(b"foo", b"bar");
        tr.set_option(TransactionOption::Timeout(500))?;
        match commit(tr).await {
            // Could time out if the commit were slow, but not before the timeout itself.
            Err(e) if e.code() == timed_out && (&mut min_elapsed).now_or_never().is_none() => {
                Err(e)
            }
            Err(e) if e.code() != timed_out => Err(e),
            _ => Ok(()),
        }
    })
    .await?;

    // (8) on_error() does not reset time from which timeout is measured
    retry_with_timeout(db, async |tr| {
        tr.set(b"foo", b"bar");
        sleep(db, 1000).await?;
        let (tr, r) = on_error(tr, TRANSACTION_TOO_OLD).await;
        r?;
        tr.set(b"foo", b"bar");
        tr.set_option(TransactionOption::Timeout(100))?;
        expect_error(commit(tr).await, timed_out, "timeout didn't fire")
    })
    .await?;

    // (9) Timeouts can be unset
    retry_with_timeout(db, async |tr| {
        tr.set(b"foo", b"bar");
        tr.set_option(TransactionOption::Timeout(100))?;
        tr.set(b"foo", b"bar");
        tr.set_option(TransactionOption::Timeout(0))?;
        sleep(db, 1000).await?;
        commit(tr).await
    })
    .await?;

    // (10) Unsetting a timeout after it has fired doesn't help
    retry_with_timeout(db, async |tr| {
        tr.set(b"foo", b"bar");
        tr.set_option(TransactionOption::Timeout(100))?;
        sleep(db, 1000).await?;
        tr.set_option(TransactionOption::Timeout(0))?;
        expect_error(commit(tr).await, timed_out, "timeout didn't fire")
    })
    .await?;

    // (11) An error thrown in commit does not reset time from which timeout is measured
    retry_with_timeout(db, async |tr| {
        let mut tr = tr;
        for i in 0..2 {
            tr.set_option(TransactionOption::Timeout(1500))?;
            tr.set_read_version(0x7FFF_FFFF_FFFF_FFF0);
            let _ = tr.get(b"foo", false).await;
            match tr.commit().await {
                Ok(c) => tr = c.reset(),
                Err(e) if i == 0 => {
                    if e.code() != FUTURE_VERSION {
                        // Something weird happened, run the transaction again.
                        return Err(error(TRANSACTION_TOO_OLD));
                    }
                    tr = e.on_error().await?;
                }
                Err(e) if e.code() != timed_out => return Err(e.into()),
                Err(e) => tr = e.reset(),
            }
        }
        Ok(())
    })
    .await
}

async fn test_db_timeouts(db: &Database) -> FdbResult<()> {
    let err = TRANSACTION_TOO_OLD;
    let timed_out = TRANSACTION_TIMED_OUT;
    db.set_option(DatabaseOption::TransactionTimeout(500))?;

    // (1) Basic timeout
    retry_with_timeout(db, async |tr| {
        sleep(db, 1000).await?;
        expect_error(commit(tr).await, timed_out, "(1) timeout didn't fire")
    })
    .await?;

    // (2) Timeout after on_error
    retry_with_timeout(db, async |tr| {
        tr.set(b"foo", b"bar");
        let (tr, r) = on_error(tr, err).await;
        r?;
        sleep(db, 1000).await?;
        let _ = tr.get(b"foo", false).await;
        expect_error(commit(tr).await, timed_out, "(2) timeout didn't fire")
    })
    .await?;

    // (3) Longer timeout than database timeout
    retry_with_timeout(db, async |tr| {
        tr.set_option(TransactionOption::Timeout(1000))?;
        sleep(db, 750).await?;
        tr.set(b"foo", b"bar");
        let (tr, r) = on_error(tr, err).await;
        r?;
        let _ = tr.get(b"foo", false).await;
        sleep(db, 750).await?;
        expect_error(commit(tr).await, timed_out, "(3) timeout didn't fire")
    })
    .await?;

    // (4) Shorter timeout than database timeout
    retry_with_timeout(db, async |tr| {
        tr.set_option(TransactionOption::Timeout(100))?;
        tr.set(b"foo", b"bar");
        sleep(db, 200).await?;
        expect_error(commit(tr).await, timed_out, "(4) timeout didn't fire")
    })
    .await?;

    // (5) Reset resets timeout to database timeout
    retry_with_timeout(db, async |tr| {
        tr.set_option(TransactionOption::Timeout(100))?;
        tr.set(b"foo", b"bar");
        sleep(db, 200).await?;
        let tr = expect_commit_error(tr.commit().await, timed_out, "(5) timeout didn't fire")?;
        let tr = tr.reset();
        tr.set(b"foo", b"bar");
        sleep(db, 200).await?;
        let (tr, r) = on_error(tr, err).await;
        r?;
        tr.set(b"foo", b"bar");
        sleep(db, 800).await?;
        expect_error(commit(tr).await, timed_out, "(5) timeout didn't fire")
    })
    .await?;

    // Back to the default, no timeout.
    db.set_option(DatabaseOption::TransactionTimeout(0))
}

async fn test_combinations(db: &Database) -> FdbResult<()> {
    // (1) Cancellation does survive on_error() even when retry limit is hit
    retry_with_timeout(db, async |tr| {
        tr.set_option(TransactionOption::RetryLimit(0))?;
        let tr = cancelled(tr.cancel());
        let (tr, r) = on_error(tr, TRANSACTION_TOO_OLD).await;
        expect_error(
            r,
            TRANSACTION_CANCELLED,
            "on_error() did not notice cancellation",
        )?;
        expect_error(
            commit(tr).await,
            TRANSACTION_CANCELLED,
            "cancellation did not survive on_error()",
        )
    })
    .await
}

fn test_predicates() {
    assert!(
        error(NOT_COMMITTED).is_retryable(),
        "unit test failed: 1020 is not retryable"
    );
    assert!(
        !error(10).is_retryable(),
        "unit test failed: 10 is retryable"
    );
}

async fn test_size_limit_option(db: &Database) -> FdbResult<()> {
    let value = vec![b'a'; 1024];
    let set_value = async |key: &[u8], limit: Option<i64>| {
        retry_with_timeout(db, async |tr| {
            if let Some(limit) = limit {
                tr.set_option(TransactionOption::SizeLimit(limit))?;
            }
            tr.set(key, &value);
            commit(tr).await
        })
        .await
    };

    set_value(b"t1", None).await?;
    let read = retry_with_timeout(db, async |tr| {
        Ok(tr.get(b"t1", false).await?.map(|v| v.to_vec()))
    })
    .await?;
    assert_eq!(read.as_deref(), Some(value.as_slice()));

    db.set_option(DatabaseOption::TransactionSizeLimit(1000))?;
    expect_error(
        set_value(b"t2", None).await,
        TRANSACTION_TOO_LARGE,
        "database size limit was ignored",
    )?;

    // Per transaction option overrides database option
    db.set_option(DatabaseOption::TransactionSizeLimit(1_000_000))?;
    expect_error(
        set_value(b"t3", Some(1000)).await,
        TRANSACTION_TOO_LARGE,
        "transaction size limit was ignored",
    )?;

    // DB default survives on_error reset
    db.set_option(DatabaseOption::TransactionSizeLimit(1000))?;
    let tr = db.create_trx()?;
    tr.set(b"t4", b"bar");
    let (tr, r) = on_error(tr, TRANSACTION_TOO_OLD).await;
    r?;
    tr.set(b"t4", &value);
    expect_error(
        commit(tr).await,
        TRANSACTION_TOO_LARGE,
        "database size limit did not survive on_error()",
    )?;

    // Reset the size limit for future tests
    db.set_option(DatabaseOption::TransactionSizeLimit(10_000_000))
}

async fn test_get_approximate_size(db: &Database) -> FdbResult<()> {
    retry_with_timeout(db, async |tr| {
        tr.set(b"key1", b"value1");
        let s1 = tr.get_approximate_size().await?;
        tr.set(b"key2", b"value2");
        let s2 = tr.get_approximate_size().await?;
        assert!(s1 < s2, "unit test failed: approximate size after set");
        tr.clear(b"key3");
        let s3 = tr.get_approximate_size().await?;
        assert!(s2 < s3, "unit test failed: approximate size after clear");
        tr.add_conflict_range(b"key3+", b"key3+\x00", ConflictRangeType::Read)?;
        let s4 = tr.get_approximate_size().await?;
        assert!(
            s3 < s4,
            "unit test failed: approximate size after read conflict"
        );
        tr.add_conflict_range(b"key4", b"key4\x00", ConflictRangeType::Write)?;
        let s5 = tr.get_approximate_size().await?;
        assert!(
            s4 < s5,
            "unit test failed: approximate size after write conflict"
        );
        commit(tr).await
    })
    .await
}

async fn test_get_client_status(db: &Database) -> FdbResult<()> {
    // A simple transaction first, so the database is initialized.
    retry_with_timeout(db, async |tr| tr.get_read_version().await.map(|_| ())).await?;
    // Only check that a meaningful report comes back. The C API tests cover the details.
    let status = db.get_client_status().await?;
    let status: String = String::from_utf8_lossy(&status)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    assert!(
        status.contains("\"Healthy\":true"),
        "unit test failed: client status is not healthy: {status}"
    );
    Ok(())
}

async fn test_range_split_points(db: &Database) -> FdbResult<()> {
    let begin = b"\x02range-split-points-a".as_slice();
    let end = b"\x02range-split-points-z".as_slice();
    retry_with_timeout(db, async |tr| {
        let split_points = tr.get_range_split_points(begin, end, 1_000_000).await?;
        assert_eq!(split_points.first().map(|k| k.key()), Some(begin));
        assert_eq!(split_points.last().map(|k| k.key()), Some(end));
        Ok(())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a running FoundationDB cluster"]
    fn unit_tests_pass() {
        let _ = env_logger::builder().is_test(true).try_init();
        futures::executor::block_on(async {
            let db = Database::new_compat(None).await.expect("cannot open fdb");
            run_unit_tests(&db).await.expect("unit tests failed");
        });
    }
}
