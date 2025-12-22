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
    sync::{Arc, Condvar, Mutex},
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
    time::Duration,
};

thread_local! {
    /// Marker for the FDB simulation thread. Only this thread is allowed to poll futures.
    static IS_FDB_THREAD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
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
            let mut queue = WAKE_QUEUE.lock().unwrap();
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

/// Wait for a wakeup to be queued (with timeout).
/// Returns true if a wakeup was queued, false on timeout.
fn wait_for_wakeup(timeout: Duration) -> bool {
    let queue = WAKE_QUEUE.lock().unwrap();
    if !queue.is_empty() {
        return true;
    }
    let (queue, result) = WAKE_CONDVAR.wait_timeout(queue, timeout).unwrap();
    !queue.is_empty() || !result.timed_out()
}

/// Handle a wake() call. If we're on the FDB thread, poll immediately.
/// Otherwise, queue the wakeup for the FDB thread to handle.
fn fdbwaker_wake(waker_ref: &FDBWaker, decrease: bool) {
    let is_fdb_thread = IS_FDB_THREAD.with(|f| f.get());

    if is_fdb_thread {
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
            let mut queue = WAKE_QUEUE.lock().unwrap();
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
    // Mark this thread as the FDB simulation thread
    IS_FDB_THREAD.with(|f| f.set(true));

    let f = UnsafeCell::new(Box::pin(future));
    let waker_arc = Arc::new(FDBWaker { f });
    let waker_ptr = Arc::into_raw(waker_arc) as *const ();
    let raw_waker = RawWaker::new(waker_ptr, &VTABLE);
    let waker = unsafe { Waker::from_raw(raw_waker) };
    waker.wake();
}

/// Block the current thread until a future completes, integrating with external async runtimes.
///
/// This function is designed for use within FDB simulation workloads when you need to
/// await futures from external runtimes like Tokio. It will block the current thread
/// and poll the future until completion, properly handling wakeups from other threads.
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
pub fn block_on_external<F: Future>(mut future: F) -> F::Output {
    // Create a simple waker that does nothing - we'll poll manually
    static NOOP_VTABLE: RawWakerVTable = RawWakerVTable::new(
        |_| RawWaker::new(std::ptr::null(), &NOOP_VTABLE),
        |_| {},
        |_| {},
        |_| {},
    );
    let raw_waker = RawWaker::new(std::ptr::null(), &NOOP_VTABLE);
    let waker = unsafe { Waker::from_raw(raw_waker) };
    let mut cx = Context::from_waker(&waker);

    // Pin the future
    // SAFETY: we're not moving the future after this
    let mut future = unsafe { Pin::new_unchecked(&mut future) };

    loop {
        // Drain any pending external wakeups first
        drain_wake_queue();

        // Poll the future
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(result) => return result,
            Poll::Pending => {
                // Wait for a wakeup from an external thread
                wait_for_wakeup(Duration::from_millis(10));
            }
        }
    }
}
