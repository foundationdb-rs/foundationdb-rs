use foundationdb_simulation::{
    register_factory, Database, Metric, Metrics, Promise, RustWorkload, RustWorkloadFactory,
    Severity, WorkloadContext, WrappedWorkload,
};

struct NoopWorkload {
    name: String,
    client_id: i32,
    context: WorkloadContext,
}

impl RustWorkload for NoopWorkload {
    fn setup(&'static mut self, _db: Database, done: Promise) {
        println!("rust_setup({}_{})", self.name, self.client_id);
        self.context.trace(
            Severity::Debug,
            "Test",
            &[("Layer", "Rust"), ("Stage", "Setup")],
        );
        done.send(true);
    }
    fn start(&'static mut self, _db: Database, done: Promise) {
        println!("rust_start({}_{})", self.name, self.client_id);
        self.context.trace(
            Severity::Debug,
            "Test",
            &[("Layer", "Rust"), ("Stage", "Start")],
        );
        done.send(true);
    }
    fn check(&'static mut self, _db: Database, done: Promise) {
        println!("rust_check({}_{})", self.name, self.client_id);
        self.context.trace(
            Severity::Debug,
            "Test",
            &[("Layer", "Rust"), ("Stage", "Check")],
        );
        done.send(true);
    }
    fn get_metrics(&self, mut out: Metrics) {
        println!("rust_get_metrics({}_{})", self.name, self.client_id);
        out.reserve(8);
        out.push(Metric::val("test", 42));
    }
    fn get_check_timeout(&self) -> f64 {
        println!("rust_get_check_timeout({}_{})", self.name, self.client_id);
        3000.
    }
}
impl NoopWorkload {
    fn new(name: String, client_id: i32, context: WorkloadContext) -> Self {
        Self {
            name,
            client_id,
            context,
        }
    }
}
impl Drop for NoopWorkload {
    fn drop(&mut self) {
        println!("rust_free({}_{})", self.name, self.client_id);
    }
}

struct NoopFactory;
impl RustWorkloadFactory for NoopFactory {
    fn create(name: String, context: WorkloadContext) -> WrappedWorkload {
        let client_id = context.client_id();
        let client_count = context.client_count();
        println!("RustWorkloadFactory::create({name})[{client_id}/{client_count}]");
        println!(
            "my_c_option: {:?}",
            context.get_option::<String>("my_c_option")
        );
        println!(
            "my_c_option: {:?}",
            context.get_option::<String>("my_c_option")
        );
        match name.as_str() {
            "NoopWorkload" => WrappedWorkload::new(NoopWorkload::new(name, client_id, context)),
            _ => panic!("Unknown workload name: {name}"),
        }
    }
}

register_factory!(NoopFactory);
