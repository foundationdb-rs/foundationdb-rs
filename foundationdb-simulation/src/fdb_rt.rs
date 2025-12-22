//! Asynchronous runtime module
//!
//! This module defines the `fdb_spawn` method to run to completion asynchronous tasks containing
//! FoundationDB futures

#![doc = include_str!("../docs/fdb_rt.md")]

use std::{
    cell::UnsafeCell,
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{Arc, Condvar, Mutex, OnceLock},
    task::{Context, Poll, RawWaker, RawWakerVTable, Wake, Waker},
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

struct FDBWaker {
    f: UnsafeCell<Pin<Box<dyn Future<Output = ()>>>>,
}

// SAFETY: We only poll `f` from the FDB simulation thread.
// Non-FDB threads only queue wakers, they don't access `f`.
unsafe impl Send for FDBWaker {}
unsafe impl Sync for FDBWaker {}

/// Poll a waker. This must only be called from the FDB simulation thread.
fn poll_waker(waker_arc: Arc<FDBWaker>) {
    let waker_ptr = Arc::into_raw(waker_arc) as *const ();
    let raw_waker = RawWaker::new(waker_ptr, &VTABLE);
    let waker = unsafe { Waker::from_raw(raw_waker) };
    let mut cx = Context::from_waker(&waker);

    let waker_ref = unsafe { &*(waker_ptr as *const FDBWaker) };
    let f = unsafe { &mut *waker_ref.f.get() };
    let _ = f.as_mut().poll(&mut cx);
    // The waker is dropped here, decreasing the refcount
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

/// Handle a wake() call. If we're on the FDB thread, poll immediately.
/// Otherwise, queue the wakeup for the FDB thread to handle.
fn fdbwaker_wake(waker_ref: &FDBWaker, decrease: bool) {
    if is_fdb_thread() {
        // We're on the FDB simulation thread - poll immediately (original behavior)
        let waker_raw = fdbwaker_clone(waker_ref);
        let waker = unsafe { Waker::from_raw(waker_raw) };
        let mut cx = Context::from_waker(&waker);
        let f = unsafe { &mut *waker_ref.f.get() };
        let _ = f.as_mut().poll(&mut cx);

        // Also drain any wakeups that were queued by other threads
        drain_wake_queue();

        if decrease {
            fdbwaker_drop(waker_ref);
        }
    } else {
        // We're on a non-FDB thread (e.g., Tokio worker) - queue the wakeup
        let waker_arc = unsafe { Arc::from_raw(waker_ref) };

        if !decrease {
            // wake_by_ref: keep the original alive
            std::mem::forget(waker_arc.clone());
        }

        {
            let mut queue = WAKE_QUEUE.lock().unwrap_or_else(|e| e.into_inner());
            queue.push_back(waker_arc);
        }
        // Notify the FDB thread that a wakeup is available
        WAKE_CONDVAR.notify_all();
    }
}

fn fdbwaker_clone(waker_ref: &FDBWaker) -> RawWaker {
    let waker_arc = unsafe { Arc::from_raw(waker_ref) };
    std::mem::forget(waker_arc.clone()); // increase ref count
    RawWaker::new(Arc::into_raw(waker_arc) as *const (), &VTABLE)
}

fn fdbwaker_drop(waker_ref: &FDBWaker) {
    unsafe { Arc::from_raw(waker_ref) };
}

const VTABLE: RawWakerVTable = unsafe {
    RawWakerVTable::new(
        |waker_ptr| fdbwaker_clone(&*(waker_ptr as *const FDBWaker)), // clone
        |waker_ptr| fdbwaker_wake(&*(waker_ptr as *const FDBWaker), true), // wake (decrease refcount)
        |waker_ptr| fdbwaker_wake(&*(waker_ptr as *const FDBWaker), false), // wake_by_ref (don't decrease refcount)
        |waker_ptr| fdbwaker_drop(&*(waker_ptr as *const FDBWaker)), // drop (decrease refcount)
    )
};

/// Spawn an async block and resolve all contained FoundationDB futures
pub fn fdb_spawn<F>(future: F)
where
    F: Future<Output = ()> + 'static,
{
    // Register this thread as the FDB simulation thread (validates single-thread invariant)
    set_fdb_thread();

    let f = UnsafeCell::new(Box::pin(future));
    let waker_arc = Arc::new(FDBWaker { f });
    let waker_ptr = Arc::into_raw(waker_arc) as *const ();
    let raw_waker = RawWaker::new(waker_ptr, &VTABLE);
    let waker = unsafe { Waker::from_raw(raw_waker) };
    waker.wake();
}

/// Signal used by `block_on_external` to receive wakeups from external runtimes.
/// Implements the `Wake` trait to properly integrate with Rust's async machinery.
struct BlockOnSignal {
    ready: Mutex<bool>,
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

/// Block the current thread until a future completes, integrating with external async runtimes.
///
/// This function is designed for use within FDB simulation workloads when you need to
/// await futures from external runtimes like Tokio. It will block the current thread
/// and poll the future until completion, properly handling wakeups from other threads.
///
/// Unlike the previous implementation that polled every 10ms, this version uses a proper
/// signaling waker that wakes up immediately when the external runtime signals completion.
///
/// # Example
/// ```ignore
/// use foundationdb_simulation::block_on_external;
///
/// let rt = tokio::runtime::Runtime::new().unwrap();
/// let _guard = rt.enter();
/// let handle = tokio::spawn(async { 42 });
/// let result = block_on_external(handle);
/// ```
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
