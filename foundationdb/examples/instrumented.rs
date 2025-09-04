//! This example demonstrates how to use the `instrumented_run` method to execute
//! a transaction and receive a detailed metrics report upon completion.
//!
//! The `instrumented_run` method is a powerful tool for monitoring and debugging
//! transaction performance, as it provides precise counters for operations, byte
//! counts for reads and writes, timing information, and support for custom metrics.

use foundationdb::{Database, FdbBindingError};

#[tokio::main]
async fn main() {
    // Safe because drop is called before the program exits
    let network = unsafe { foundationdb::boot() };

    // Run the instrumented example
    if let Err(e) = instrumented_example().await {
        eprintln!("Error running instrumented example: {e:?}");
    }

    // Shutdown the client
    drop(network);
}

async fn instrumented_example() -> Result<(), FdbBindingError> {
    let db = Database::default()?;

    println!("Running an instrumented transaction...");

    // Use `instrumented_run` to execute a transaction and get metrics.
    let result = db
        .instrumented_run(|txn, _| async move {
            // Perform a few simple operations
            txn.set(b"instrumented_key", b"instrumented_value");
            let _ = txn.get(b"instrumented_key", false).await?;

            // Register a custom metric directly on the transaction
            txn.set_custom_metric("operations_count", 1, &[("type", "write")])?;

            Ok(())
        })
        .await;

    // Handle the result and print the metrics report
    match result {
        Ok((_, metrics)) => {
            println!("Transaction successful!");
            println!("--- Metrics Report ---");
            println!("{metrics:#?}");
            println!("----------------------");
        }
        Err((err, metrics)) => {
            eprintln!("Transaction failed: {err:?}");
            eprintln!("--- Metrics Report (on failure) ---");
            println!("{metrics:#?}");
            println!("-----------------------------------");
            return Err(err);
        }
    }

    Ok(())
}

/*
// Example output:

Running an instrumented transaction...
Transaction successful!
--- Metrics Report ---
MetricsReport {
    current: CounterMetrics {
        call_atomic_op: 0,
        call_clear: 0,
        call_clear_range: 0,
        call_get: 1,
        keys_values_fetched: 1,
        bytes_read: 34,
        call_set: 1,
        bytes_written: 34,
    },
    total: CounterMetrics {
        call_atomic_op: 0,
        call_clear: 0,
        call_clear_range: 0,
        call_get: 1,
        keys_values_fetched: 1,
        bytes_read: 34,
        call_set: 1,
        bytes_written: 34,
    },
    time: TimingMetrics {
        commit_execution_ms: 2,
        on_error_execution_ms: [],
        total_execution_ms: 4,
    },
    custom_metrics: {
        MetricKey {
            name: "operations_count",
            labels: [
                (
                    "type",
                    "write",
                ),
            ],
        }: 1,
    },
    transaction: TransactionInfo {
        retries: 0,
        read_version: None,
        commit_version: Some(
            21848159754,
        ),
    },
}
----------------------
*/
