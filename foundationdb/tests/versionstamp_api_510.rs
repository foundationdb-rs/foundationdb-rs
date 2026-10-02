use foundationdb::{Database, api::FdbApiBuilder, options::MutationType};
use std::panic::{AssertUnwindSafe, catch_unwind};

// API selection is process-global, so this regression has its own test binary.
#[tokio::test]
async fn versionstamped_mutations_reject_runtime_api_510() {
    FdbApiBuilder::default()
        .set_runtime_version(510)
        .build()
        .expect("select API 510");
    let db = Database::new_compat(None).await.expect("open database");
    let trx = db.create_trx().expect("create transaction");
    for mutation in [
        MutationType::SetVersionstampedKey,
        MutationType::SetVersionstampedValue,
    ] {
        assert!(
            catch_unwind(AssertUnwindSafe(
                || trx.atomic_op(b"key", b"value", mutation)
            ))
            .is_err()
        );
    }
    // The legacy API remains usable for other atomic mutations.
    trx.atomic_op(b"counter", &1_i64.to_le_bytes(), MutationType::Add);
}
