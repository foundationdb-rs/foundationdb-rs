# FDB Simulation Async Runtime

This document explains how the `fdb_rt` module provides async execution for FoundationDB
simulation workloads, including the threading model, waker infrastructure, and integration
with external runtimes like Tokio.

## Architecture Overview

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│                         FDB Simulation Process                               │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  ┌──────────────────────────────┐    ┌──────────────────────────────────┐   │
│  │   FDB Simulation Thread      │    │     External Runtime Threads      │   │
│  │   (single, deterministic)    │    │     (e.g., Tokio workers)         │   │
│  │                              │    │                                   │   │
│  │  ┌────────────────────────┐  │    │  ┌─────────────────────────────┐  │   │
│  │  │     fdb_spawn()        │  │    │  │    tokio::spawn()           │  │   │
│  │  │  Polls FDBWaker        │  │    │  │  Runs async tasks           │  │   │
│  │  └───────────┬────────────┘  │    │  └──────────────┬──────────────┘  │   │
│  │              │               │    │                 │                 │   │
│  │              │ poll()        │    │                 │ wake()          │   │
│  │              ▼               │    │                 ▼                 │   │
│  │  ┌────────────────────────┐  │    │                                   │   │
│  │  │     drain_wake_queue() │◄─┼────┼──── WAKE_QUEUE + WAKE_CONDVAR ◄──│   │
│  │  │  Process queued wakes  │  │    │     (cross-thread signaling)     │   │
│  │  └────────────────────────┘  │    │                                   │   │
│  │                              │    │                                   │   │
│  │  FDB_THREAD_ID: OnceLock     │    │                                   │   │
│  │  (validates single-thread)   │    │                                   │   │
│  └──────────────────────────────┘    └──────────────────────────────────┘   │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

## Threading Model

FDB simulation is **strictly single-threaded**. All future polling must occur on the
simulation thread to maintain determinism. The `fdb_rt` module enforces this invariant:

- **`FDB_THREAD_ID`**: A `OnceLock<ThreadId>` that stores the simulation thread's ID
- **`set_fdb_thread()`**: Registers the current thread as the FDB thread (called by `fdb_spawn`)
- **`is_fdb_thread()`**: Checks if the current thread is the FDB simulation thread

This detection is critical for safe integration with multi-threaded runtimes.

## The `fdb_spawn` Function

`fdb_spawn` is the entry point for running async code in the simulation. It takes a future
and drives it to completion using the FDB event loop.

```rust
pub fn fdb_spawn<F>(future: F)
where
    F: Future<Output = ()> + 'static,
{
    // Register this thread as the FDB simulation thread
    set_fdb_thread();

    // Wrap the future in an FDBWaker
    let f = UnsafeCell::new(Box::pin(future));
    let waker_arc = Arc::new(FDBWaker { f });

    // Convert to a standard Waker using the Wake trait
    let waker: Waker = waker_arc.into();

    // Start execution by calling wake()
    waker.wake();
}
```

### How It Works

1. **Thread Registration**: `set_fdb_thread()` records the current thread ID
2. **Future Wrapping**: The future is pinned and wrapped in an `FDBWaker` with an `Arc`
3. **Waker Creation**: The `Arc<FDBWaker>` is converted to a `Waker` via the `Wake` trait
4. **Initial Poll**: Calling `wake()` triggers the first poll of the future

## The `FDBWaker`

`FDBWaker` wraps a future and implements the `Wake` trait for polling.

```rust
struct FDBWaker {
    f: UnsafeCell<Pin<Box<dyn Future<Output = ()>>>>,
}

// SAFETY: We only poll `f` from the FDB simulation thread.
// Non-FDB threads only queue wakers, they don't access `f`.
unsafe impl Send for FDBWaker {}
unsafe impl Sync for FDBWaker {}
```

### Thread Safety

The `UnsafeCell` provides interior mutability for the pinned future. While `FDBWaker`
implements `Send + Sync`, this is safe because:

1. The future is **only polled from the FDB simulation thread**
2. Non-FDB threads only interact with the `Arc` wrapper and queue mechanism
3. The `wake_impl` method detects which thread it's on and acts accordingly

### Wake Behavior

The `Wake` trait implementation handles wakeups differently based on the calling thread:

```rust
impl FDBWaker {
    fn wake_impl(self: Arc<Self>) {
        if is_fdb_thread() {
            // On FDB thread: poll immediately (original behavior)
            let waker: Waker = self.clone().into();
            let mut cx = Context::from_waker(&waker);
            let f = unsafe { &mut *self.f.get() };
            let _ = f.as_mut().poll(&mut cx);

            // Also drain any wakeups queued by other threads
            drain_wake_queue();
        } else {
            // On non-FDB thread: queue the wakeup
            {
                let mut queue = WAKE_QUEUE.lock().unwrap();
                queue.push_back(self);
            }
            // Notify the FDB thread
            WAKE_CONDVAR.notify_all();
        }
    }
}
```

## Cross-Thread Wake Queue

When external runtimes (like Tokio) call `wake()` from their worker threads, the wakeup
must be safely forwarded to the FDB simulation thread.

```rust
/// Queue for pending wakeups from non-FDB threads
static WAKE_QUEUE: Mutex<VecDeque<Arc<FDBWaker>>> = Mutex::new(VecDeque::new());

/// Condition variable to notify the FDB thread
static WAKE_CONDVAR: Condvar = Condvar::new();
```

### The `drain_wake_queue` Function

Called from the FDB thread to process all pending wakeups:

```rust
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
```

## The `block_on_external` Function (EXPERIMENTAL)

> **WARNING: Experimental Feature**
>
> Using multiple async runtimes is generally **a bad idea** and `block_on_external` is
> **an ugly hack** for when you cannot modify a library that depends on Tokio. The ideal
> solution is always to avoid external runtimes in simulation workloads entirely.
>
> **Use with extreme caution.**

### When This Is Actually Useful

A valid use case is integrating with libraries that internally require `tokio::spawn` for
distributed/parallel execution.

In these cases, you cannot modify the library to use FDB-compatible async primitives.
The `block_on_external` function provides a bridge to execute these workloads within
simulation, understanding that the external runtime portion is not deterministic.

`block_on_external` allows blocking on futures from external runtimes like Tokio:

```rust
pub fn block_on_external<F: Future>(future: F) -> F::Output {
    let signal = Arc::new(BlockOnSignal {
        ready: Mutex::new(false),
        condvar: Condvar::new(),
    });
    let waker: Waker = signal.clone().into();
    let mut cx = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);

    loop {
        // Drain any pending FDBWaker wakeups
        drain_wake_queue();

        // Reset ready flag before polling
        {
            let mut ready = signal.ready.lock().unwrap();
            *ready = false;
        }

        // Poll the future
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(result) => return result,
            Poll::Pending => {
                // Wait for external runtime to call wake()
                let mut ready = signal.ready.lock().unwrap();
                while !*ready {
                    ready = signal.condvar.wait(ready).unwrap();
                }
            }
        }
    }
}
```

### The `BlockOnSignal` Struct

A signaling mechanism that implements `Wake` for external runtime integration:

```rust
struct BlockOnSignal {
    ready: Mutex<bool>,
    condvar: Condvar,
}

impl Wake for BlockOnSignal {
    fn wake(self: Arc<Self>) {
        let mut ready = self.ready.lock().unwrap();
        *ready = true;
        self.condvar.notify_one();
    }
}
```

## Complete Flow Examples

### Pure FDB Async Flow

When awaiting only FDB operations:

```text
fdb_spawn(async { db.get(key).await })
│
├─► FDBWaker created with pinned future
│
├─► wake() called (on FDB thread)
│   │
│   └─► poll() the future
│       │
│       └─► FdbFuture::poll() returns Pending
│           │
│           └─► Registers callback with fdb_future_set_callback()
│
├─► Control returns to fdbserver event loop
│
├─► fdbserver resolves the future internally
│
├─► Callback fires: fdb_future_callback()
│   │
│   └─► wake() called (still on FDB thread)
│       │
│       └─► poll() the future
│           │
│           └─► FdbFuture::poll() returns Ready(value)
│
└─► Future completes, FDBWaker dropped
```

### Mixed FDB + Tokio Flow

When using `block_on_external` with Tokio:

```text
fdb_spawn(async {
    let result = block_on_external(tokio::spawn(async { 42 }));
})
│
├─► FDBWaker created for outer async block
│
├─► wake() called, poll() the outer future
│   │
│   └─► block_on_external() entered
│       │
│       ├─► BlockOnSignal created (ready=false)
│       │
│       ├─► poll() the JoinHandle
│       │   │
│       │   └─► Pending (task not complete)
│       │
│       └─► Wait on condvar...
│
├─► [Meanwhile, on Tokio worker thread]
│   │
│   └─► Task completes, Tokio calls wake() on BlockOnSignal
│       │
│       └─► Sets ready=true, notifies condvar
│
├─► block_on_external wakes up
│   │
│   ├─► drain_wake_queue() (process any FDBWaker wakeups)
│   │
│   └─► poll() the JoinHandle
│       │
│       └─► Ready(42)
│
├─► block_on_external returns 42
│
└─► Outer future completes
```

## FDB Future Integration

The FDB simulation executor integrates with FoundationDB's C API through callbacks.
When an `FdbFuture` is polled:

1. If not ready, it registers a callback via `fdb_future_set_callback`
2. The callback receives an `AtomicWaker` that holds our `FDBWaker`
3. When the fdbserver resolves the future, the callback fires
4. The callback calls `wake()` on our waker, triggering re-poll
5. This time `fdb_future_is_ready` returns true, and the value is extracted

This allows Rust async/await syntax to work seamlessly with the FDB simulation
event loop, maintaining determinism as long as you only use FDB-compatible futures.
