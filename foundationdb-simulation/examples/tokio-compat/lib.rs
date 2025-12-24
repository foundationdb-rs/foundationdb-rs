//! # Tokio Compatibility Example
//!
//! > **WARNING: Experimental Feature - Use with Extreme Caution**
//! >
//! > Using multiple async runtimes is generally **a bad idea** and this is
//! > **an ugly hack** for when you cannot modify a library that depends on Tokio.
//! > The ideal solution is always to avoid Tokio in simulation workloads.
//!
//! This example demonstrates how to use Tokio alongside FDB simulation workloads
//! using the [`block_on_external`] function.
//!
//! ## The Problem
//!
//! FDB simulation is strictly single-threaded. When you spawn a Tokio task, it runs
//! on Tokio's worker threads. If you try to `.await` a `JoinHandle` directly, Tokio
//! will call `wake()` from a worker thread, violating the single-thread invariant
//! and potentially causing undefined behavior.
//!
//! ## The Solution
//!
//! The [`block_on_external`] function provides a safe bridge:
//!
//! 1. It creates a `Waker` that uses a condvar for signaling
//! 2. Polls the Tokio future
//! 3. When Tokio calls `wake()` from a worker thread, it signals the condvar
//! 4. The FDB thread wakes up and re-polls
//!
//! ## Valid Use Cases
//!
//! This pattern is useful when integrating with libraries that internally require
//! `tokio::spawn` for distributed/parallel execution, such as:
//!
//! - Query engines (e.g., DataFusion) that parallelize execution via Tokio tasks
//! - gRPC clients that spawn background tasks for connection management
//! - HTTP clients with internal worker threads
//!
//! ## Key Patterns Demonstrated
//!
//! - **Static Runtime**: Use [`OnceLock<Runtime>`] to lazily initialize Tokio
//! - **Enter Guard**: Use `rt.enter()` to set the Tokio context before spawning
//! - **block_on_external**: Use this instead of `.await` for Tokio futures
//!
//! ## Running This Example
//!
//! ```bash
//! cargo build --release --example tokio_compat
//! fdbserver -r simulation -f foundationdb-simulation/examples/tokio-compat/test_file_74.toml
//! ```

use std::sync::OnceLock;
use tokio::runtime::Runtime;

use foundationdb_simulation::{
    block_on_external, details, register_workload, Metric, Metrics, RustWorkload, Severity,
    SimDatabase, SingleRustWorkload, WorkloadContext,
};

/// Global Tokio runtime, lazily initialized.
///
/// We use [`OnceLock`] instead of `lazy_static` for:
/// - No extra dependency
/// - Explicit initialization timing
/// - Thread-safe single initialization
///
/// The runtime persists for the entire simulation lifetime.
static TOKIO_RUNTIME: OnceLock<Runtime> = OnceLock::new();

/// Get or create the global Tokio runtime.
///
/// Creates a multi-threaded runtime with 2 worker threads.
/// The runtime is initialized lazily on first access and persists
/// for the entire simulation.
fn get_runtime() -> &'static Runtime {
    TOKIO_RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .build()
            .expect("Failed to create Tokio runtime")
    })
}

/// Example workload demonstrating Tokio integration with FDB simulation.
///
/// This workload spawns tasks on Tokio worker threads and uses [`block_on_external`]
/// to safely await their completion from the FDB simulation thread.
pub struct TokioCompatWorkload {
    /// Workload context for tracing and configuration.
    context: WorkloadContext,
    /// The ID of this client instance.
    client_id: i32,
    /// Number of Tokio tasks to spawn (configurable via test file).
    task_count: usize,
    /// Counter for tasks that have been spawned.
    tasks_spawned: usize,
    /// Counter for tasks that have completed successfully.
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

            // PATTERN 1: Enter Tokio runtime context.
            // This is REQUIRED so that tokio::spawn knows which runtime to use.
            // The guard must be held while spawning the task.
            let _guard = rt.enter();

            // PATTERN 2: Spawn task on Tokio worker thread.
            // This task will run asynchronously on Tokio's thread pool,
            // completely separate from the FDB simulation thread.
            let handle = tokio::spawn(async move {
                // yield_now() ensures we actually context-switch to a worker thread,
                // demonstrating that the cross-thread wake mechanism works correctly.
                tokio::task::yield_now().await;
                // Simulate some CPU-bound computation
                (0..100i32).sum::<i32>()
            });

            self.tasks_spawned += 1;

            // PATTERN 3: Use block_on_external to wait for the Tokio task.
            //
            // This blocks the FDB simulation thread but properly handles wakeups
            // from Tokio worker threads via a condvar-based signaling mechanism.
            //
            // WITHOUT this, directly awaiting `handle` would cause cross-thread wake
            // violations and potential undefined behavior, because Tokio would call
            // wake() on our FDBWaker from a worker thread.
            match block_on_external(handle) {
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
