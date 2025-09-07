use foundationdb as fdb;
use rand::distr::Alphanumeric;
use rand::Rng;
use std::sync::Once;

/// Initialize tracing for tests
#[allow(unused)]
pub fn init_tracing() {
    static INIT: Once = Once::new();

    INIT.call_once(|| {
        #[cfg(feature = "trace")]
        {
            tracing_subscriber::fmt()
                .with_max_level(tracing::Level::TRACE)
                .with_test_writer()
                .try_init()
                .ok();
        }
    });
}

/// generate random string. Foundationdb watch only fires when value changed, so updating with same
/// value twice will not fire watches. To make examples work over multiple run, we use random
/// string as a value.
#[allow(unused)]
pub fn random_str(len: usize) -> String {
    rand::rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

#[allow(unused)]
pub async fn database() -> fdb::FdbResult<fdb::Database> {
    fdb::Database::new_compat(None).await
}
