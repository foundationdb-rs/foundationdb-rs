use std::sync::OnceLock;
use tokio::runtime::Runtime;

use foundationdb_simulation::{
    details, register_workload, Metric, Metrics, RustWorkload, Severity, SimDatabase,
    SingleRustWorkload, WorkloadContext,
};

static TOKIO_RUNTIME: OnceLock<Runtime> = OnceLock::new();

fn get_runtime() -> &'static Runtime {
    TOKIO_RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("Failed to create Tokio runtime")
    })
}

pub struct TokioCompatWorkload {
    context: WorkloadContext,
    client_id: i32,
    task_count: usize,
    tasks_spawned: usize,
    tasks_completed: usize,
}

impl SingleRustWorkload for TokioCompatWorkload {
    fn new(_name: String, context: WorkloadContext) -> Self {
        Self {
            client_id: context.client_id(),
            task_count: context.get_option("task_count").unwrap_or(5),
            context,
            tasks_spawned: 0,
            tasks_completed: 0,
        }
    }
}

impl RustWorkload for TokioCompatWorkload {
    async fn setup(&mut self, _db: SimDatabase) {
        println!("tokio_compat_setup({})", self.client_id);

        // Initialize runtime during setup
        let _ = get_runtime();

        self.context.trace(
            Severity::Info,
            "TokioCompat setup complete",
            details![
                "Layer" => "Rust",
                "Client" => self.client_id
            ],
        );
    }

    async fn start(&mut self, _db: SimDatabase) {
        println!("tokio_compat_start({})", self.client_id);

        // Only run on client 0
        if self.client_id != 0 {
            return;
        }

        let rt = get_runtime();

        for i in 0..self.task_count {
            self.context.trace(
                Severity::Info,
                "Spawning Tokio task",
                details![
                    "Layer" => "Rust",
                    "Client" => self.client_id,
                    "TaskIndex" => i
                ],
            );

            // Enter Tokio runtime context
            let _guard = rt.enter();

            // Spawn task on Tokio worker thread.
            // This is where the bug manifests - when this task completes,
            // the Tokio worker thread will call wake() on the FDBWaker
            // from a different thread, causing undefined behavior.
            let handle = tokio::spawn(async move {
                // Yield to ensure we actually run on a worker thread
                tokio::task::yield_now().await;
                // Simulate some computation
                (0..100i32).sum::<i32>()
            });

            self.tasks_spawned += 1;

            // Awaiting triggers cross-thread wake
            match handle.await {
                Ok(result) => {
                    assert_eq!(result, (0..100i32).sum::<i32>());
                    self.tasks_completed += 1;
                    self.context.trace(
                        Severity::Info,
                        "Tokio task completed",
                        details![
                            "Layer" => "Rust",
                            "Client" => self.client_id,
                            "TaskIndex" => i,
                            "Result" => result
                        ],
                    );
                }
                Err(e) => {
                    self.context.trace(
                        Severity::Error,
                        "Tokio task failed",
                        details![
                            "Layer" => "Rust",
                            "Client" => self.client_id,
                            "TaskIndex" => i,
                            "Error" => e.to_string()
                        ],
                    );
                }
            }
        }
    }

    async fn check(&mut self, _db: SimDatabase) {
        println!("tokio_compat_check({})", self.client_id);

        if self.client_id != 0 {
            return;
        }

        if self.tasks_completed == self.task_count {
            self.context.trace(
                Severity::Info,
                "All Tokio tasks completed successfully",
                details![
                    "Layer" => "Rust",
                    "TaskCount" => self.task_count,
                    "Completed" => self.tasks_completed
                ],
            );
        } else {
            self.context.trace(
                Severity::Error,
                "Task completion count mismatch",
                details![
                    "Layer" => "Rust",
                    "Expected" => self.task_count,
                    "Completed" => self.tasks_completed
                ],
            );
        }
    }

    fn get_metrics(&self, mut out: Metrics) {
        println!("tokio_compat_get_metrics({})", self.client_id);
        out.extend([
            Metric::val("task_count", self.task_count as f64),
            Metric::val("tasks_spawned", self.tasks_spawned as f64),
            Metric::val("tasks_completed", self.tasks_completed as f64),
        ]);
    }

    fn get_check_timeout(&self) -> f64 {
        println!("tokio_compat_get_check_timeout({})", self.client_id);
        5000.0
    }
}

register_workload!(TokioCompatWorkload);
