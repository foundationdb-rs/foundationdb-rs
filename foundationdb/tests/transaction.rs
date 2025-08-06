use foundationdb::{options::MutationType, FdbResult};

mod common;

#[test]
fn test_should_commit() {
    let _guard = unsafe { foundationdb::boot() };
    futures::executor::block_on(test_should_commit_reset()).expect("failed to run");
    futures::executor::block_on(test_should_commit_write_operations()).expect("failed to run");
}

async fn test_should_commit_reset() -> FdbResult<()> {
    let db = common::database().await?;

    let mut trx = db.create_trx()?;
    assert!(!trx.should_commit());

    trx.set(b"hello", b"world");
    assert!(trx.should_commit());

    trx.reset();
    assert!(!trx.should_commit());
   
    Ok(())
}

async fn test_should_commit_write_operations() -> FdbResult<()> {
    let db = common::database().await?;

    let mut trx = db.create_trx()?;

    assert!(!trx.should_commit());
    trx.set(b"hello", b"world");
    assert!(trx.should_commit());
    trx.reset();
    
    assert!(!trx.should_commit());
    trx.clear(b"hello");
    assert!(trx.should_commit());
    trx.reset();

    assert!(!trx.should_commit());
    trx.atomic_op(b"hello", b"world", MutationType::Add);
    assert!(trx.should_commit());
    trx.reset();

    assert!(!trx.should_commit());
    trx.clear_range(b"hello", b"world");
    assert!(trx.should_commit());
    trx.reset();

    Ok(())
}
