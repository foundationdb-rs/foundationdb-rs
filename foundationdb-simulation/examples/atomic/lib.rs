use foundationdb_macros::simulation_entrypoint;
use foundationdb_simulation::{RustWorkload, WorkloadContext};

mod workload;

use workload::AtomicWorkload;

#[simulation_entrypoint]
pub fn simulated_main(name: &str, context: WorkloadContext) -> Box<dyn RustWorkload> {
    match name {
        "AtomicWorkload" => Box::new(AtomicWorkload::new(context)),
        name => panic!("no workload with name: {:?}", name),
    }
}
