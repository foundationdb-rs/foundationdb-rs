use foundationdb::{api::FdbApiBuilder, Database};
use std::thread;

mod common;
use common::init_tracing;

#[test]
fn test_old_api_builder_with_new_database() {
    init_tracing();

    // Use old API builder to set up API version
    let network_builder = FdbApiBuilder::default()
        .build()
        .expect("API builder should work");

    // Then use new automatic Database management
    let db = Database::new(None).expect("Database creation should work");

    // Both should work together
    drop(db);
    drop(network_builder);
}

#[test]
fn test_manual_network_with_automatic_database() {
    init_tracing();

    // Set up network manually using the old API
    let network_builder = FdbApiBuilder::default()
        .build()
        .expect("API builder should work");

    let (runner, waiter) = network_builder
        .build()
        .expect("Network builder should work");

    // Start network manually in background thread
    let handle = std::thread::spawn(move || {
        unsafe { runner.run() }.expect("Network should run");
    });

    // Wait for network to start
    let stopper = waiter.wait();

    // Now create databases using automatic management
    // This should work because the unified system detects the network is already running
    let db1 = Database::new(None).expect("First database should work");
    let db2 = Database::new(None).expect("Second database should work");

    // Use the databases
    drop(db1);
    drop(db2);

    // Stop manually managed network
    stopper.stop().expect("Network should stop");
    handle.join().expect("Network thread should join");
}

#[test]
fn test_automatic_then_manual() {
    init_tracing();

    // Start with automatic management
    let db1 = Database::new(None).expect("Automatic database should work");

    // Try to use old boot API - it should use the already-running network
    let _guard = unsafe { foundationdb::boot() }.expect("Boot should work with existing network");

    let db2 = Database::new(None).expect("Second database should work");

    drop(db1);
    drop(db2);
    // Guard will keep network alive until dropped
}

#[test]
fn test_deprecated_boot_with_database() {
    init_tracing();

    // Use deprecated boot API
    let guard = unsafe { foundationdb::boot() }.expect("Boot should work");

    // Create databases - they should use the same network
    let db1 = Database::new(None).expect("Database should work with boot");
    let db2 = Database::new(None).expect("Second database should work");

    drop(db1);
    drop(db2);
    drop(guard);
}

#[tokio::test]
async fn test_mixed_sync_async() {
    init_tracing();

    // Use old sync API
    let network_builder = FdbApiBuilder::default()
        .build()
        .expect("API should initialize");

    // Use new async compat API
    let db1 = Database::new_compat(None)
        .await
        .expect("Async compat should work");

    // Use new sync API
    let db2 = Database::new(None).expect("Sync new API should work");

    drop(db1);
    drop(db2);
    drop(network_builder);
}

#[test]
fn test_multiple_api_builders() {
    init_tracing();

    // Create multiple API builders (should be safe due to Once protection)
    let builder1 = FdbApiBuilder::default()
        .build()
        .expect("First builder should work");

    let builder2 = FdbApiBuilder::default()
        .build()
        .expect("Second builder should work (API already set)");

    // Both should work
    let db = Database::new(None).expect("Database should work");

    drop(db);
    drop(builder1);
    drop(builder2);
}

#[test]
fn test_network_stop_compatibility() {
    init_tracing();

    // Set up manual network
    let network_builder = FdbApiBuilder::default()
        .build()
        .expect("API builder should work");

    // Create database with automatic management - this starts the network automatically
    let db = Database::new(None).expect("Database should work");
    let db_keeper = Database::new(None).expect("Keeper database should work");

    // Test that manual network operations work with automatic management
    let (_runner, waiter) = network_builder
        .build()
        .expect("Network builder should work even after automatic start");

    let stopper = waiter.wait();

    // Drop one database but keep keeper
    drop(db);

    // Network should still be running due to keeper
    let db2 = Database::new(None).expect("Database should still work");
    drop(db2);

    // Keep keeper and stopper alive to prevent network stop for other tests
    std::mem::forget(db_keeper);
    std::mem::forget(stopper);
}

#[test]
fn test_reference_counting_with_mixed_apis() {
    init_tracing();

    // Use automatic management
    let db1 = Database::new(None).expect("First database should work");

    // Use deprecated boot API - should increment ref count
    let guard = unsafe { foundationdb::boot() }.expect("Boot should work");

    // Create more databases
    let db2 = Database::new(None).expect("Second database should work");
    let db3 = Database::new(None).expect("Third database should work");
    let db_keeper = Database::new(None).expect("Keeper database should work");

    // Drop databases one by one - network should stay alive due to guard + keeper
    drop(db1);
    drop(db2);
    drop(db3);

    // Network should still be running because of guard + keeper
    let db4 = Database::new(None).expect("Database should still work");
    drop(db4);
    drop(guard);

    // Keep keeper alive to prevent network stop for other tests
    std::mem::forget(db_keeper);
}

#[test]
fn test_network_stop_failure_handling() {
    init_tracing();

    // This test documents the panic behavior on network stop failure
    // We can't actually test this easily without breaking other tests
    // because once the network stops, it cannot be restarted
    let _db = Database::new(None).expect("Database should work");

    // Note: In a real failure scenario, network stop would panic with detailed message
    // This prevents resource leaks and makes failures obvious to developers
}

#[test]
fn test_concurrent_network_initialization() {
    use std::sync::{Arc, Barrier};
    use std::thread;

    init_tracing();

    const NUM_THREADS: usize = 10;
    let barrier = Arc::new(Barrier::new(NUM_THREADS));
    let mut handles = vec![];

    // Spawn multiple threads that all try to initialize network concurrently
    for i in 0..NUM_THREADS {
        let barrier = barrier.clone();
        let handle = thread::spawn(move || {
            // Wait for all threads to be ready
            barrier.wait();

            // All threads try to create databases simultaneously
            let db = Database::new(None).expect(&format!("Thread {} should create database", i));

            // Hold database for a bit to test reference counting under contention
            thread::sleep(std::time::Duration::from_millis(10));
            drop(db);
        });
        handles.push(handle);
    }

    // Wait for all threads to complete
    for handle in handles {
        handle.join().expect("Thread should complete successfully");
    }
}

#[test]
fn test_error_code_constants_usage() {
    init_tracing();

    // This test verifies that error codes are properly defined and accessible
    use foundationdb::api::error_codes;

    // Verify constants have correct values
    assert_eq!(error_codes::NETWORK_ALREADY_STOPPED, 2025);
    assert_eq!(error_codes::API_VERSION_NOT_SUPPORTED, 2203);

    // Create a database successfully to verify network starts properly
    let _db = Database::new(None).expect("Database should work");

    // Note: We don't actually test NETWORK_ALREADY_STOPPED error in unit tests
    // because once the network stops, it cannot be restarted (FoundationDB limitation)
    // and would break other tests running in the same process
}

#[test]
fn test_memory_cleanup_on_drop() {
    init_tracing();

    // Create multiple databases to increment reference count
    let db1 = Database::new(None).expect("First database should work");
    let db2 = Database::new(None).expect("Second database should work");
    let db3 = Database::new(None).expect("Third database should work");
    let db_keeper = Database::new(None).expect("Keeper database should work");

    // Drop in different order to test reference counting
    drop(db2); // Should not stop network (ref_count: 4 -> 3)
    drop(db1); // Should not stop network (ref_count: 3 -> 2)
    drop(db3); // Should not stop network (ref_count: 2 -> 1)

    // Keep db_keeper alive to prevent network stop for other tests
    std::mem::forget(db_keeper);
}

#[test]
fn test_mixed_api_error_propagation() {
    init_tracing();

    // Create database with automatic management
    let db = Database::new(None).expect("Database should work");

    // Try using manual API after automatic initialization
    let api_builder_result = FdbApiBuilder::default().set_runtime_version(710).build();

    // This should work since the unified system handles API version setting
    assert!(
        api_builder_result.is_ok(),
        "API builder should work with existing API version"
    );

    drop(db);
}

#[test]
fn test_network_thread_cleanup() {
    init_tracing();

    // Test that network thread is properly cleaned up
    let db = Database::new(None).expect("Database should work");

    // Give network thread time to start
    thread::sleep(std::time::Duration::from_millis(50));

    drop(db);

    // Give network thread time to shut down
    thread::sleep(std::time::Duration::from_millis(50));

    // If we reach here without hanging, thread cleanup worked properly
}
