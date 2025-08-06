use foundationdb::{FdbBindingError, FdbError};
use std::error::Error;
use std::fmt::Formatter;
use std::sync::atomic::AtomicU8;

#[cfg(feature = "trace")]
#[test]
fn test_trace() {
    use tracing_subscriber::fmt::format::FmtSpan;

    let _guard = unsafe { foundationdb::boot() };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let test_writer = tracing_subscriber::fmt::TestWriter::new();

    let _e = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_span_events(FmtSpan::FULL)
        .with_writer(test_writer)
        .try_init();

    rt.block_on(async {
        test_traces_on_run().await;
    });
}

#[cfg(feature = "trace")]
#[tracing::instrument(skip_all)]
async fn test_traces_on_run() {
    let db = foundationdb::Database::new_compat(None)
        .await
        .expect("failed to open fdb");

    db.run(|trx, _b| async move {
        trx.set("tracing".as_ref(), "ok".as_ref());
        Ok(())
    })
    .await
    .expect("could not run transaction");

    // Test closure error
    #[derive(Debug)]
    struct CustomError;

    impl std::fmt::Display for CustomError {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            write!(f, "{self:?}")
        }
    }
    impl Error for CustomError {}

    let result = db
        .run(|_trx, _b| async move {
            return Err::<(), FdbBindingError>(FdbBindingError::CustomError(Box::new(CustomError)));
        })
        .await;

    assert!(result.is_err());

    // test transaction kept error
    let result = db
        .run(|trx, _b| async move {
            let trx_clone = trx.clone();
            Ok(trx_clone)
        })
        .await;
    assert!(result.is_err());
    if let Err(e) = result {
        assert!(matches!(e, FdbBindingError::ReferenceToTransactionKept));
    }

    // Test retryable errors
    let attempt: AtomicU8 = AtomicU8::new(0);
    let result = db
        .run(|_trx, _| async {
            let new_val = &attempt.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if *new_val < 2 {
                return Err(FdbError::from_code(1020).into());
            }
            Ok(())
        })
        .await;
    assert!(result.is_ok());

    // cleaning up
    db.run(|trx, _b| async move {
        trx.clear("tracing".as_ref());
        Ok(())
    })
    .await
    .expect("could not run transaction");
}
