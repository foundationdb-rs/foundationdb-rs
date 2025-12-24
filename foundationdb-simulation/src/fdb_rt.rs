//! # FDB Simulation Async Runtime
//!
//! This module provides the async execution infrastructure for FoundationDB simulation workloads.
//!
//! ## Threading Model
//!
//! FDB simulation is **strictly single-threaded**. All future polling must occur on the
//! simulation thread to maintain determinism. This module enforces this invariant and provides
//! mechanisms to safely integrate with multi-threaded runtimes like Tokio.
//!
//! ## Key Components
//!
//! - [`fdb_spawn`]: Spawn a future to be driven by the FDB simulation event loop
//! - [`block_on_external`]: Block until an external runtime's future completes (experimental)
//! - [`drain_wake_queue`]: Process pending wakeups from other threads
//!
//! ## When to Use What
//!
//! | Scenario | Function |
//! |----------|----------|
//! | Awaiting FDB operations | `fdb_spawn` (automatic via workload phases) |
//! | Awaiting Tokio tasks | [`block_on_external`] |
//! | Processing queued wakeups | [`drain_wake_queue`] (usually automatic) |
//!
//! ## Detailed Documentation
//!
//! For a comprehensive explanation of the architecture, threading model, and flow diagrams,
//! see the included documentation below.

#![doc = include_str!("../docs/fdb_rt.md")]

use std::{
    cell::UnsafeCell,
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{Arc, Condvar, Mutex, OnceLock},
    task::{Context, Poll, Wake, Waker},
    thread::{self, ThreadId},
};

/// Stores the thread ID of the FDB simulation thread.
/// Only this thread is allowed to poll futures directly.
static FDB_THREAD_ID: OnceLock<ThreadId> = OnceLock::new();

/// Register the current thread as the FDB simulation thread.
/// Panics if called from a different thread than a previous call.
fn set_fdb_thread() {
    let current = thread::current().id();
    match FDB_THREAD_ID.get() {
        Some(stored) if *stored != current => {
            panic!(
                "fdb_spawn called from multiple threads! First: {stored:?}, Current: {current:?}"
            );
        }
        Some(_) => {} // Same thread, OK
        None => {
            FDB_THREAD_ID.get_or_init(|| current);
        }
    }
}

/// Check if the current thread is the FDB simulation thread.
fn is_fdb_thread() -> bool {
    FDB_THREAD_ID
        .get()
        .map(|id| *id == thread::current().id())
        .unwrap_or(false)
}

/// Thread-safe queue for pending wakeups from non-FDB threads.
/// When wake() is called from a Tokio worker thread, the waker is queued here
/// and will be polled later by the FDB simulation thread.
static WAKE_QUEUE: Mutex<VecDeque<Arc<FDBWaker>>> = Mutex::new(VecDeque::new());

/// Condition variable to notify the FDB thread when a wakeup is queued.
static WAKE_CONDVAR: Condvar = Condvar::new();

/// Waker implementation for FDB simulation futures.
///
/// Wraps a pinned future and provides the [`Wake`] trait implementation needed
/// to integrate with Rust's async machinery.
///
/// # Thread Safety
///
/// This struct uses [`UnsafeCell`] for interior mutability of the pinned future.
/// Despite implementing `Send + Sync`, this is safe because:
///
/// 1. The future is **only polled from the FDB simulation thread**
/// 2. Non-FDB threads only interact with the [`Arc`] wrapper and queue mechanism
/// 3. The [`wake_impl`](Self::wake_impl) method detects which thread it's on and acts accordingly
///
/// # Wake Behavior
///
/// - **On FDB thread**: Polls the future immediately (original single-threaded behavior)
/// - **On other threads**: Queues the waker for later polling by the FDB thread via [`WAKE_QUEUE`]
struct FDBWaker {
    /// The wrapped future. Only accessed from the FDB simulation thread.
    f: UnsafeCell<Pin<Box<dyn Future<Output = ()>>>>,
}

// SAFETY: We only poll `f` from the FDB simulation thread.
// Non-FDB threads only queue wakers, they don't access `f`.
// The wake_impl() method checks is_fdb_thread() before accessing `f`.
unsafe impl Send for FDBWaker {}
unsafe impl Sync for FDBWaker {}

/// Poll a waker. This must only be called from the FDB simulation thread.
fn poll_waker(waker_arc: Arc<FDBWaker>) {
    let waker: Waker = waker_arc.clone().into();
    let mut cx = Context::from_waker(&waker);
    let f = unsafe { &mut *waker_arc.f.get() };
    let _ = f.as_mut().poll(&mut cx);
}

/// Drain the wake queue and poll all pending futures.
/// Must only be called from the FDB simulation thread.
/// Returns true if any futures were polled.
pub fn drain_wake_queue() -> bool {
    let mut polled_any = false;
    loop {
        let waker_arc = {
            let mut queue = WAKE_QUEUE.lock().unwrap_or_else(|e| e.into_inner());
            queue.pop_front()
        };

        match waker_arc {
            Some(arc) => {
                poll_waker(arc);
                polled_any = true;
            }
            None => break,
        }
    }
    polled_any
}

impl Wake for FDBWaker {
    fn wake(self: Arc<Self>) {
        self.wake_impl();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.clone().wake_impl();
    }
}

impl FDBWaker {
    /// Handle a wake() call. If we're on the FDB thread, poll immediately.
    /// Otherwise, queue the wakeup for the FDB thread to handle.
    fn wake_impl(self: Arc<Self>) {
        if is_fdb_thread() {
            // We're on the FDB simulation thread - poll immediately (original behavior)
            let waker: Waker = self.clone().into();
            let mut cx = Context::from_waker(&waker);
            let f = unsafe { &mut *self.f.get() };
            let _ = f.as_mut().poll(&mut cx);

            // Also drain any wakeups that were queued by other threads
            drain_wake_queue();
        } else {
            // We're on a non-FDB thread (e.g., Tokio worker) - queue the wakeup
            {
                let mut queue = WAKE_QUEUE.lock().unwrap_or_else(|e| e.into_inner());
                queue.push_back(self);
            }
            // Notify the FDB thread that a wakeup is available
            WAKE_CONDVAR.notify_all();
        }
    }
}

/// Spawn an async block and resolve all contained FoundationDB futures
pub fn fdb_spawn<F>(future: F)
where
    F: Future<Output = ()> + 'static,
{
    // Register this thread as the FDB simulation thread (validates single-thread invariant)
    set_fdb_thread();

    let f = UnsafeCell::new(Box::pin(future));
    let waker_arc = Arc::new(FDBWaker { f });
    let waker: Waker = waker_arc.into();
    waker.wake();
}

/// Signaling mechanism for [`block_on_external`].
///
/// This struct implements the [`Wake`] trait to receive notifications from external
/// async runtimes (like Tokio) when their futures are ready to make progress.
///
/// # How It Works
///
/// 1. [`block_on_external`] creates a `BlockOnSignal` with `ready = false`
/// 2. A [`Waker`] is created from this signal and passed to the external future
/// 3. When the external runtime completes work, it calls `wake()`
/// 4. `wake()` sets `ready = true` and notifies the condvar
/// 5. [`block_on_external`] wakes up and re-polls the future
///
/// # Thread Safety
///
/// This struct is designed to be called from external runtime threads (e.g., Tokio workers).
/// The mutex is accessed with `unwrap_or_else` to avoid panicking on poison, since worker
/// threads may panic independently.
struct BlockOnSignal {
    /// Flag indicating whether a wake signal has been received.
    ready: Mutex<bool>,
    /// Condition variable used to block the FDB thread until wake() is called.
    condvar: Condvar,
}

impl Wake for BlockOnSignal {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        // Use unwrap_or_else to avoid panicking if mutex is poisoned.
        // This is critical because wake() is called from Tokio worker threads.
        let mut ready = self.ready.lock().unwrap_or_else(|e| e.into_inner());
        *ready = true;
        self.condvar.notify_one();
    }
}

/// Block the current thread until a future from an external runtime completes.
///
/// # WARNING: Experimental Feature
///
/// Using multiple async runtimes is generally **a bad idea** and `block_on_external`
/// is **an ugly hack** for when you cannot modify a library that depends on Tokio.
/// The ideal solution is always to avoid external runtimes in simulation workloads.
///
/// **Use with extreme caution.**
///
/// # Purpose
///
/// FDB simulation uses a custom single-threaded executor that cooperates with the
/// fdbserver event loop. External runtimes like Tokio have their own executors
/// with worker thread pools. This function bridges the two worlds.
///
/// Valid use cases include libraries that internally require `tokio::spawn` for
/// distributed/parallel execution (e.g., DataFusion query execution, gRPC clients).
///
/// # Why Not Just `.await`?
///
/// Directly awaiting a Tokio `JoinHandle` inside an FDB workload would:
///
/// 1. **Block the FDB thread** - The Tokio future's waker would be called from
///    a Tokio worker thread
/// 2. **Cause cross-thread polling** - The `FDBWaker::wake()` would be called
///    from the wrong thread
/// 3. **Violate safety invariants** - Potentially cause undefined behavior
///
/// # Example
///
/// ```ignore
/// use foundationdb_simulation::block_on_external;
/// use tokio::runtime::Runtime;
/// use std::sync::OnceLock;
///
/// static RT: OnceLock<Runtime> = OnceLock::new();
///
/// fn get_runtime() -> &'static Runtime {
///     RT.get_or_init(|| {
///         tokio::runtime::Builder::new_multi_thread()
///             .worker_threads(2)
///             .build()
///             .unwrap()
///     })
/// }
///
/// // Inside a workload:
/// let rt = get_runtime();
/// let _guard = rt.enter();
/// let handle = tokio::spawn(async { expensive_computation() });
/// let result = block_on_external(handle).unwrap();
/// ```
///
/// # Caveats
///
/// - **Blocks the FDB thread**: While waiting, no FDB simulation progress occurs
/// - **Not for FDB futures**: Use regular `.await` for FDB operations
/// - **Runtime must be initialized**: You need a running Tokio runtime
/// - **Non-deterministic**: The external runtime portion is not simulation-deterministic
pub fn block_on_external<F: Future>(future: F) -> F::Output {
    // Create a signaling waker that notifies via condvar when wake() is called
    let signal = Arc::new(BlockOnSignal {
        ready: Mutex::new(false),
        condvar: Condvar::new(),
    });
    let waker: Waker = signal.clone().into();
    let mut cx = Context::from_waker(&waker);

    // Pin the future on the stack
    let mut future = std::pin::pin!(future);

    loop {
        // Drain any pending external wakeups first (for FDBWaker futures)
        drain_wake_queue();

        // Reset ready flag BEFORE polling, so any wake() during poll is captured
        {
            let mut ready = signal.ready.lock().unwrap_or_else(|e| e.into_inner());
            *ready = false;
        }

        // Poll the future
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(result) => return result,
            Poll::Pending => {
                // Wait for wake() to be called by the external runtime
                let mut ready = signal.ready.lock().unwrap_or_else(|e| e.into_inner());
                while !*ready {
                    ready = signal
                        .condvar
                        .wait(ready)
                        .unwrap_or_else(|e| e.into_inner());
                }
            }
        }
    }
}
