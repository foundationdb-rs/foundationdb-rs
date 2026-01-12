use foundationdb_simulation::{
    register_workload, Metrics, RustWorkload, SimDatabase, SingleRustWorkload, WorkloadContext,
};
use futures_util::FutureExt;

pub struct SharedWorkload {
    client_id: i32,
}

impl SingleRustWorkload for SharedWorkload {
    fn new(_name: String, context: WorkloadContext) -> Self {
        Self {
            client_id: context.client_id(),
        }
    }
}

impl RustWorkload for SharedWorkload {
    async fn setup(&mut self, _db: SimDatabase) {
        println!("rust_setup({})", self.client_id);
    }
    async fn start(&mut self, db: SimDatabase) {
        println!("rust_start({})", self.client_id);
        let trx = db.create_trx().expect("Could not create transaction");
        let future = trx.get_read_version();
        let shared = future.shared();
        let version = shared.await;
        println!("read_version({}) = {version:?}", self.client_id);
    }
    async fn check(&mut self, _db: SimDatabase) {
        println!("rust_check({})", self.client_id);
    }
    fn get_metrics(&self, mut _out: Metrics) {
        println!("rust_get_metrics({})", self.client_id);
    }
    fn get_check_timeout(&self) -> f64 {
        println!("rust_get_check_timeout({})", self.client_id);
        5000.0
    }
}

register_workload!(SharedWorkload);
