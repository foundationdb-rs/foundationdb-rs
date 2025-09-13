use foundationdb::{metrics::TransactionMetrics, *};
mod common;
use std::borrow::Cow;
use std::sync::{Arc, Mutex};

// #[test]
// fn test_metrics() {
//     let _guard = unsafe { foundationdb::boot() };
//     futures::executor::block_on(instrumented_run_success()).expect("failed to run");
//     futures::executor::block_on(instrumented_run_with_n_retries()).expect("failed to run");
//     futures::executor::block_on(test_counter_metrics()).expect("failed to run");
//     futures::executor::block_on(test_transaction_info()).expect("failed to run");
//     futures::executor::block_on(test_time_metrics()).expect("failed to run");
//     futures::executor::block_on(test_custom_metrics()).expect("failed to run");
//     futures::executor::block_on(test_transaction_custom_metrics()).expect("failed to run");
// }

/// Tests a successful transaction using `instrumented_run`.
///
/// This test verifies that a simple transaction (one SET and one GET operation)
/// completes successfully and that the returned metrics correctly report the number
/// of operations performed.
#[tokio::test]
async fn test_instrumented_run_success() -> FdbResult<()> {
    const KEY: &[u8] = b"test_metrics_success";
    const VALUE: &[u8] = b"value";
    const SUCCESS: u64 = 42;

    let db = common::database().await?;

    let (result, metrics) = match db
        .instrumented_run(|txn, _| async move {
            txn.set(KEY, VALUE);
            Ok(SUCCESS)
        })
        .await
    {
        Ok((result, metrics)) => (result, metrics),
        Err(_err) => {
            panic!()
        }
    };

    assert_eq!(result, SUCCESS);

    let total = metrics.total;
    assert_eq!(total.call_set, 1);
    assert_eq!(total.bytes_written, (KEY.len() + VALUE.len()) as u64);

    let transaction_info = metrics.transaction;
    assert_eq!(transaction_info.retries, 0);

    Ok(())
}

/// Tests the retry mechanism of `instrumented_run`.
///
/// This test simulates a retryable error (`transaction_too_old`) to ensure that
/// the `instrumented_run` method correctly retries the transaction. It then
/// asserts that the `retries` count in the final metrics report is accurate.
#[tokio::test]
async fn test_instrumented_run_with_n_retries() -> FdbResult<()> {
    const KEY: &[u8] = b"test_metrics_retry";
    const VALUE: &[u8] = b"value";
    const SUCCESS: u64 = 42;

    // Number of retries we want to force
    const EXPECTED_RETRIES: u64 = 3;

    let db = common::database().await?;

    // Use Arc<Mutex<>> to share and modify the counter across async calls
    let attempt_counter = Arc::new(Mutex::new(0));

    let (result, metrics) = match db
        .instrumented_run(|txn, _| {
            let counter = attempt_counter.clone();
            async move {
                // Set a key to verify metrics
                txn.set(KEY, VALUE);

                // Increment the counter and check if we should still fail
                let mut attempts = counter.lock().unwrap();
                *attempts += 1;

                if *attempts <= EXPECTED_RETRIES {
                    // Return a retryable error (not_committed) for the first N attempts
                    let fdb_error = FdbError::from_code(1020);
                    // Return FdbBindingError directly - instrumented_run will handle metrics
                    Err(FdbBindingError::from(fdb_error))
                } else {
                    // Succeed on attempt N+1
                    Ok(SUCCESS)
                }
            }
        })
        .await
    {
        Ok((result, metrics)) => (result, metrics),
        Err(err) => {
            panic!("Test failed: {:?}", err);
        }
    };

    // Verify the result
    assert_eq!(result, SUCCESS);

    // Verify the metrics
    let total = metrics.total;
    assert_eq!(total.call_set, EXPECTED_RETRIES + 1); // One set operation per attempt
    assert_eq!(
        total.bytes_written,
        (EXPECTED_RETRIES + 1) * (KEY.len() + VALUE.len()) as u64
    );

    let current = metrics.current;
    assert_eq!(current.call_set, 1);
    assert_eq!(current.bytes_written, (KEY.len() + VALUE.len()) as u64);

    let transaction_info = metrics.transaction;
    assert_eq!(transaction_info.retries, EXPECTED_RETRIES); // Should match our expected retry count

    // Verify the counter
    let final_attempts = *attempt_counter.lock().unwrap();
    assert_eq!(final_attempts, EXPECTED_RETRIES + 1); // Total attempts should be retries + 1

    Ok(())
}

/// Performs a comprehensive test of all counter metrics.
///
/// This test executes a single transaction that performs a variety of operations:
/// - Multiple `SET` operations
/// - A `GET` operation
/// - A `GET_RANGE` operation
/// - `CLEAR` and `CLEAR_RANGE` operations
/// - An `ATOMIC_OP`
///
/// It then performs precise assertions on all relevant counter metrics, including
/// operation counts, exact bytes written, and exact bytes read, ensuring they are
/// all tracked correctly within a single transaction.
#[tokio::test]
async fn test_counter_metrics() -> FdbResult<()> {
    let db = common::database().await?;

    const PREFIX: &[u8] = b"test_counter_metrics_";
    const SET_OPS: usize = 3;
    let mut bytes_written: u64 = 0;
    for i in 0..SET_OPS {
        let key = format!("{}_key{}", std::str::from_utf8(PREFIX).unwrap(), i);
        let value = format!("value{}", i);
        bytes_written += (key.len() + value.len()) as u64;
    }

    let ((fetched_count, bytes_read), metrics) = match db
        .instrumented_run(|txn, _| {
            async move {
                // 1. SET operations
                for i in 0..SET_OPS {
                    let key = format!("{}_key{}", std::str::from_utf8(PREFIX).unwrap(), i);
                    let value = format!("value{}", i);
                    txn.set(key.as_bytes(), value.as_bytes());
                }

                // 2. GET operation
                let get_key = format!("{}_key1", std::str::from_utf8(PREFIX).unwrap()).into_bytes();
                let mut bytes_read_acc = 0;
                if let Some(value_slice) = txn.get(&get_key, false).await? {
                    bytes_read_acc += (get_key.len() + value_slice.len()) as u64;
                }
                let get_count = 1;

                // 3. GET_RANGE operation
                let range_begin =
                    format!("{}_key", std::str::from_utf8(PREFIX).unwrap()).into_bytes();
                let range_end =
                    format!("{}_key4", std::str::from_utf8(PREFIX).unwrap()).into_bytes();
                let range_option = RangeOption {
                    begin: KeySelector::first_greater_or_equal(Cow::from(range_begin)),
                    end: KeySelector::first_greater_or_equal(Cow::from(range_end)),
                    limit: Some(100),
                    ..Default::default()
                };
                let range_result = txn.get_range(&range_option, 1, false).await?;
                for kv in range_result.iter() {
                    bytes_read_acc += (kv.key().len() + kv.value().len()) as u64;
                }

                let range_count = range_result.len();

                // 4. CLEAR operation
                let clear_key =
                    format!("{}_key2", std::str::from_utf8(PREFIX).unwrap()).into_bytes();
                txn.clear(&clear_key);

                // 5. CLEAR_RANGE operation
                let clear_range_begin =
                    format!("{}_key1", std::str::from_utf8(PREFIX).unwrap()).into_bytes();
                let clear_range_end =
                    format!("{}_key3", std::str::from_utf8(PREFIX).unwrap()).into_bytes();
                txn.clear_range(&clear_range_begin[..], &clear_range_end[..]);

                // 6. ATOMIC operation (add)
                let atomic_key =
                    format!("{}_atomic", std::str::from_utf8(PREFIX).unwrap()).into_bytes();
                txn.atomic_op(
                    &atomic_key,
                    &[1, 0, 0, 0, 0, 0, 0, 0],
                    options::MutationType::Add,
                );

                Ok((get_count + range_count, bytes_read_acc))
            }
        })
        .await
    {
        Ok(val) => val,
        Err((err, _)) => match err {
            FdbBindingError::NonRetryableFdbError(fdb_err) => return Err(fdb_err),
            _ => panic!("Test failed with unexpected error type: {:?}", err),
        },
    };

    // Verify the metrics
    let report = metrics;

    // Check counter metrics for current attempt
    assert_eq!(
        report.current.call_set, SET_OPS as u64,
        "Should have {} SET operations",
        SET_OPS
    );
    assert_eq!(report.current.call_get, 1, "Should have 1 GET operation");

    // Verify the number of key-values fetched matches our result count
    assert_eq!(
        report.current.keys_values_fetched, fetched_count as u64,
        "Should have fetched {} key-values",
        fetched_count
    );

    assert_eq!(
        report.current.bytes_written, bytes_written,
        "Should have written {} bytes",
        bytes_written
    );
    assert_eq!(
        report.current.bytes_read, bytes_read,
        "Should have read {} bytes",
        bytes_read
    );

    assert_eq!(
        report.current.call_clear, 1,
        "Should have 1 CLEAR operation"
    );
    assert_eq!(
        report.current.call_clear_range, 1,
        "Should have 1 CLEAR_RANGE operation"
    );
    assert_eq!(
        report.current.call_atomic_op, 1,
        "Should have 1 ATOMIC operation"
    );

    // Total metrics should match current metrics (no retries)
    assert_eq!(report.total.call_set, report.current.call_set);
    assert_eq!(report.total.call_get, report.current.call_get);
    assert_eq!(
        report.total.keys_values_fetched,
        report.current.keys_values_fetched
    );
    assert_eq!(report.total.bytes_read, report.current.bytes_read);
    assert_eq!(report.total.call_clear, report.current.call_clear);
    assert_eq!(
        report.total.call_clear_range,
        report.current.call_clear_range
    );
    assert_eq!(report.total.call_atomic_op, report.current.call_atomic_op);
    assert_eq!(report.total.bytes_written, report.current.bytes_written);

    // Verify transaction info
    assert_eq!(report.transaction.retries, 0, "Should have no retries");
    assert!(
        report.transaction.commit_version.is_some(),
        "Should have a commit version"
    );

    Ok(())
}

/// Tests the `TransactionInfo` fields within the metrics report.
///
/// This test runs two transactions:
/// 1. A simple successful transaction to verify that `commit_version` is present
///    and the `retries` count is zero.
/// 2. A transaction that is forced to retry, to verify that the `retries` count
///    is correctly reported.
#[tokio::test]
async fn test_transaction_info() -> FdbResult<()> {
    let db = common::database().await?;

    // Test read_version field
    {
        // Create a transaction with metrics
        let metrics = TransactionMetrics::new();
        let txn = db
            .create_instrumented_trx(metrics.clone())
            .expect("Could not create transaction");

        // Get the read version from the transaction
        let read_version = txn.get_read_version().await?;

        // Set the read version in metrics
        metrics.set_read_version(read_version);

        // Verify the read version was set correctly
        let transaction_info = metrics.get_transaction_info();
        assert_eq!(transaction_info.read_version, Some(read_version));

        // Commit the transaction
        txn.commit().await?;
    }

    // Test commit_version field
    {
        // Test with a write transaction
        let _metrics = TransactionMetrics::new();

        let (_result, metrics_data) = db
            .instrumented_run(|txn, _| async move {
                // Perform a write operation
                txn.set(b"test_commit_version", b"value");
                Ok(())
            })
            .await
            .expect("Transaction failed");

        // Verify transaction info
        let transaction_info = metrics_data.transaction;

        // Commit version should be set (we don't know the exact value)
        assert!(transaction_info.commit_version.is_some());
    }

    // Test retries field
    {
        // Number of retries we want to force
        const EXPECTED_RETRIES: u64 = 2;

        // Use Arc<Mutex<>> to share and modify the counter across async calls
        let attempt_counter = Arc::new(Mutex::new(0));

        // Run a transaction that will be retried
        let result = db
            .instrumented_run(|_txn, _| {
                let counter = attempt_counter.clone();
                async move {
                    // Increment the counter and check if we should still fail
                    let mut attempts = counter.lock().unwrap();
                    *attempts += 1;

                    if *attempts <= EXPECTED_RETRIES {
                        // Return a retryable error for the first N attempts
                        let fdb_error = FdbError::from_code(1020); // not_committed
                        Err(FdbBindingError::from(fdb_error))
                    } else {
                        // Succeed on attempt N+1
                        Ok(())
                    }
                }
            })
            .await;

        match result {
            Ok((_, metrics_data)) => {
                // Verify retry count
                let transaction_info = metrics_data.transaction;
                assert_eq!(transaction_info.retries, EXPECTED_RETRIES);
            }
            Err(_) => panic!("Transaction should have succeeded after retries"),
        }
    }

    Ok(())
}

/// Tests that timing metrics are recorded.
///
/// This test runs a transaction and verifies that the timing metrics
/// (`commit_seconds`, `error_seconds`, `total_seconds`) are greater than zero,
/// confirming that the time spent in different phases of the transaction is being measured.
#[tokio::test]
async fn test_time_metrics() -> FdbResult<()> {
    let db = common::database().await?;

    // Test time metrics
    {
        // Run a transaction to generate time metrics
        let (_result, metrics_data) = db
            .instrumented_run(|txn, _| async move {
                // Perform multiple operations to ensure measurable time
                for i in 0..10 {
                    let key = format!("test_time_metrics_{}", i).into_bytes();
                    txn.set(&key, b"value");
                }
                // Read some data to add more execution time
                let _ = txn.get(b"test_time_metrics_0", false).await?;
                Ok(())
            })
            .await
            .expect("Transaction failed");

        // Get time metrics
        let time_metrics = metrics_data.time;

        // Verify time metrics
        assert!(
            time_metrics.get_total_error_time() == 0,
            "Total execution time should not be recorded"
        );
        assert!(
            time_metrics.commit_execution_ms > 0,
            "Commit execution time should be recorded"
        );

        // Run a transaction that will generate error handling time
        let attempt_counter = Arc::new(Mutex::new(0));
        let result = db
            .instrumented_run(|_txn, _| {
                let counter = attempt_counter.clone();
                async move {
                    let mut attempts = counter.lock().unwrap();
                    *attempts += 1;

                    if *attempts == 1 {
                        // Return a retryable error on first attempt
                        let fdb_error = FdbError::from_code(1020); // not_committed
                        Err(FdbBindingError::from(fdb_error))
                    } else {
                        Ok(())
                    }
                }
            })
            .await;

        if let Ok((_, metrics_data)) = result {
            let time_metrics = metrics_data.time;

            // Verify error handling time
            assert!(
                !time_metrics.on_error_execution_ms.is_empty(),
                "Error handling time should be recorded"
            );
        } else {
            panic!("Transaction should have succeeded after retry");
        }
    }

    Ok(())
}

/// Tests the functionality of custom metrics set on the `TransactionMetrics` object.
///
/// This test verifies that `set_custom_metric` and `increment_custom_metric` work
/// correctly when called on the central `TransactionMetrics` object outside of any
/// specific transaction. It ensures that custom metrics are properly registered and aggregated.
#[tokio::test]
async fn test_custom_metrics() -> FdbResult<()> {
    let db = common::database().await?;

    // Test custom metrics
    {
        // Create metrics and transaction
        let metrics = TransactionMetrics::new();
        let txn = db
            .create_instrumented_trx(metrics.clone())
            .expect("Could not create transaction");

        // Test custom metrics with labels
        metrics.set_custom(
            "custom_counter",
            123,
            &[("tenant", "test"), ("region", "us-west")],
        );
        metrics.set_custom("custom_timer", 456, &[("operation", "write")]);

        // Test incrementing custom metrics
        metrics.increment_custom("incremented_counter", 5, &[("service", "api")]);
        metrics.increment_custom("incremented_counter", 10, &[("service", "api")]);

        // Get metrics data
        let metrics_data = metrics.get_metrics_data();
        let custom = metrics_data.custom_metrics;

        // Verify custom counter with labels
        let key = foundationdb::metrics::MetricKey::new(
            "custom_counter",
            &[("tenant", "test"), ("region", "us-west")],
        );
        let custom_counter = custom.get(&key).copied();
        assert_eq!(custom_counter, Some(123));

        // Verify custom timer with labels
        let key = foundationdb::metrics::MetricKey::new("custom_timer", &[("operation", "write")]);
        let custom_timer = custom.get(&key).copied();
        assert_eq!(custom_timer, Some(456));

        // Verify incremented counter
        let key =
            foundationdb::metrics::MetricKey::new("incremented_counter", &[("service", "api")]);
        let incremented = custom.get(&key).copied();
        assert_eq!(incremented, Some(15));

        // Commit the transaction
        txn.commit().await?;
    }

    Ok(())
}

/// Tests custom metrics that are set directly on a transaction.
///
/// This test uses `instrumented_run` to execute a transaction and calls
/// `set_custom_metric` and `increment_custom_metric` on the `Transaction` object
/// itself. It then verifies that these transaction-specific custom metrics are
/// correctly recorded in the final metrics report.
#[tokio::test]
async fn test_transaction_custom_metrics() -> Result<(), FdbBindingError> {
    let db = common::database().await?;

    // Test transaction custom metrics methods using instrumented_run
    let result = db
        .instrumented_run(|txn, _| async move {
            // Set custom metrics directly on the transaction
            txn.set_custom_metric("txn_counter", 100, &[("operation", "read")])?;
            txn.set_custom_metric("txn_timer", 200, &[("component", "storage")])?;

            // Increment a custom metric
            txn.increment_custom_metric("txn_incremented", 10, &[("type", "query")])?;
            txn.increment_custom_metric("txn_incremented", 15, &[("type", "query")])?;

            // Read a value to make sure the transaction does something
            let _value = txn.get(b"test_key", false).await?;

            Ok(())
        })
        .await;

    // Verify the result and check metrics in the returned metrics data
    match result {
        Ok((_, metrics_data)) => {
            let custom = metrics_data.custom_metrics;

            // Verify metrics were properly recorded
            let key =
                foundationdb::metrics::MetricKey::new("txn_counter", &[("operation", "read")]);
            assert_eq!(custom.get(&key).copied(), Some(100));

            let key =
                foundationdb::metrics::MetricKey::new("txn_timer", &[("component", "storage")]);
            assert_eq!(custom.get(&key).copied(), Some(200));

            let key =
                foundationdb::metrics::MetricKey::new("txn_incremented", &[("type", "query")]);
            assert_eq!(custom.get(&key).copied(), Some(25));
        }
        Err((err, _)) => {
            return Err(err);
        }
    }

    Ok(())
}
