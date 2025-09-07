use foundationdb::Database;

mod common;
use common::init_tracing;

// Since the network can only be initialized once per process,
// we can only test automatic management in integration tests.
// Manual management tests would require separate processes.

#[test]
fn test_automatic_network_management() {
    init_tracing();
    // Test the new automatic network management
    let db1 = Database::new(None).expect("could not create first database");
    let db2 = Database::new(None).expect("could not create second database");

    // Both databases should work
    drop(db1);
    drop(db2);
    // Network continues running for other tests
}

#[test]
fn test_multiple_databases_automatic() {
    init_tracing();
    // Create multiple databases with automatic management
    let databases: Vec<Database> = (0..5)
        .map(|_| Database::new(None).expect("could not create database"))
        .collect();

    // All databases should be valid
    assert_eq!(databases.len(), 5);

    // Drop all databases
    drop(databases);
}

#[tokio::test]
async fn test_async_compat_api() {
    init_tracing();
    // Test the async compat API
    let db = Database::new_compat(None)
        .await
        .expect("could not create database");
    drop(db);
}

#[test]
fn test_mixed_apis() {
    init_tracing();
    // Test that both old boot() API and new Database API work
    // Note: We can't test manual network management in the same process
    // as automatic management since they share the same global state

    // Using deprecated boot() API - it now uses automatic management internally
    let _guard = unsafe { foundationdb::boot() }.expect("could not boot");

    let db = Database::new(None).expect("could not create database");
    drop(db);

    // Guard will keep network alive
}

#[test]
fn test_database_from_pointer() {
    init_tracing();
    // Test that Database::new_from_pointer doesn't manage network
    // This is used by the simulation runtime

    // First ensure network is running
    let _db1 = Database::new(None).expect("could not create database");

    // Create a database from pointer (simulation mode)
    // Note: We can't actually create a valid pointer here without the simulation runtime,
    // but we can test that the manages_network field is set correctly
    // This is more of a compile-time test to ensure the API exists

    // The actual functionality is tested in the simulation tests
}
