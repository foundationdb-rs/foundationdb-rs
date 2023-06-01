use foundationdb::FdbBindingError;

#[test]
// This test is here because I'm always creating infinite recursion on Display and Debug impl 🤦
// Exhibit A: https://github.com/foundationdb-rs/foundationdb-rs/pull/83
// Exhibit B: https://github.com/foundationdb-rs/foundationdb-rs/issues/93
fn test_debug_display_trait() {
    let error = FdbBindingError::ReferenceToTransactionKept;
    println!("{}", error);
    println!("{:?}", error);
}
