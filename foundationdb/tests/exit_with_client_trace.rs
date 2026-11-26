//! Regression test for foundationdb-rs#585: with client tracing enabled, a program
//! that relies on the automatic network stop at process exit must exit cleanly.
//! See "Stopping the network at process exit" in `foundationdb/src/api.rs`.
//!
//! The crash only shows at process exit, so the test re-runs this binary as child
//! processes and checks their exit status. `harness = false`: the children need
//! control over which thread starts the network and which thread exits, which
//! libtest does not give.

use std::path::Path;
use std::process::{Command, ExitStatus};
use std::time::Duration;

use foundationdb::api::FdbApiBuilder;
use foundationdb::options::NetworkOption;
use foundationdb::{Database, FdbBindingError};

const TEST_NAME: &str = "exit_with_client_trace";
const CHILD_MODE_ENV: &str = "FDB_RS_EXIT_WITH_CLIENT_TRACE_MODE";
const CHILD_TRACE_DIR_ENV: &str = "FDB_RS_EXIT_WITH_CLIENT_TRACE_DIR";
// Generous: a child does one transaction against a local cluster. Counted in
// polling steps rather than read from a clock.
const CHILD_POLL_INTERVAL: Duration = Duration::from_millis(50);
const CHILD_MAX_POLLS: u32 = 1200; // 60s

/// How a child process uses FoundationDB before exiting.
const MODES: &[&str] = &[
    // Boot and use FDB on the main thread, return from main.
    "main-thread",
    // Boot and use FDB on a spawned thread, like libtest or `tokio::spawn`,
    // then return from main.
    "spawned-thread",
];
const RUNS_PER_MODE: usize = 3;

fn main() {
    if let (Some(mode), Some(trace_dir)) = (
        std::env::var_os(CHILD_MODE_ENV),
        std::env::var_os(CHILD_TRACE_DIR_ENV),
    ) {
        return child(mode.to_str().expect("utf-8 mode"), Path::new(&trace_dir));
    }

    // Minimal libtest argument handling, so that `cargo test <filter>`,
    // `--list` and `--ignored` behave as expected.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--list") {
        println!("{TEST_NAME}: test");
        return;
    }
    if args.iter().any(|a| a == "--ignored") {
        return;
    }
    let filters: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    if !filters.is_empty() && !filters.iter().any(|f| TEST_NAME.contains(f.as_str())) {
        return;
    }

    parent();
}

fn parent() {
    let exe = std::env::current_exe().expect("current exe");
    for mode in MODES {
        for run in 0..RUNS_PER_MODE {
            let trace_dir = std::env::temp_dir().join(format!(
                "fdb-rs-{TEST_NAME}-{}-{mode}-{run}",
                std::process::id()
            ));
            std::fs::create_dir_all(&trace_dir).expect("failed to create the trace dir");

            let mut child = Command::new(&exe)
                .env(CHILD_MODE_ENV, mode)
                .env(CHILD_TRACE_DIR_ENV, &trace_dir)
                .spawn()
                .expect("failed to spawn the child process");
            let status = wait_with_timeout(&mut child);
            let trace = read_trace_files(&trace_dir);
            let _ = std::fs::remove_dir_all(&trace_dir);

            let Some(status) = status else {
                panic!(
                    "{mode} run {run}: the child did not exit in time (is a cluster reachable?)"
                );
            };
            assert!(
                status.success(),
                "{mode} run {run}: the child must exit cleanly with client tracing enabled, got {status}"
            );
            // A clean stop flushes the stop events and closes the trace file.
            for expected in ["ClientStopNetwork", "RunNetworkTerminating", "</Trace>"] {
                assert!(
                    trace.contains(expected),
                    "{mode} run {run}: the trace file must contain {expected:?}"
                );
            }
        }
    }
    println!("{TEST_NAME}: ok");
}

/// Waits for the child, killing it after `CHILD_MAX_POLLS` (e.g. no reachable
/// cluster). Returns `None` on timeout.
fn wait_with_timeout(child: &mut std::process::Child) -> Option<ExitStatus> {
    for _ in 0..CHILD_MAX_POLLS {
        if let Some(status) = child.try_wait().expect("failed to wait for the child") {
            return Some(status);
        }
        std::thread::sleep(CHILD_POLL_INTERVAL);
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

/// Concatenates every XML trace file of the directory.
fn read_trace_files(dir: &Path) -> String {
    let mut trace = String::new();
    for entry in std::fs::read_dir(dir).expect("failed to read the trace dir") {
        let path = entry.expect("trace dir entry").path();
        if path.extension().is_some_and(|ext| ext == "xml") {
            trace.push_str(&std::fs::read_to_string(&path).expect("failed to read a trace file"));
        }
    }
    trace
}

/// Uses FoundationDB with tracing on, then exits without stopping the network:
/// the exit-time stop must handle it.
fn child(mode: &str, trace_dir: &Path) {
    let trace_dir = trace_dir.to_str().expect("utf-8 trace dir").to_string();
    match mode {
        "main-thread" => use_fdb(trace_dir),
        "spawned-thread" => std::thread::spawn(move || use_fdb(trace_dir))
            .join()
            .expect("the FDB thread panicked"),
        other => panic!("unknown child mode {other:?}"),
    }
}

fn use_fdb(trace_dir: String) {
    FdbApiBuilder::default()
        .build()
        .expect("failed to select fdb api")
        .set_option(NetworkOption::TraceEnable(trace_dir))
        .expect("failed to enable client tracing")
        .boot()
        .expect("failed to boot fdb");

    futures::executor::block_on(async {
        let db = Database::new_compat(None)
            .await
            .expect("failed to open fdb");
        db.run(|trx, _| async move {
            trx.set(b"exit_with_client_trace", b"ok");
            Ok::<_, FdbBindingError>(())
        })
        .await
        .expect("failed to commit");
    });
}
