Here is an explanation of how `fdb_spawn` communicates with the simulation through the
foundationdb-rs SDK.
```rs
    fdb_spawn(async move {
        ... // code that relies on polling FdbFutures
    }
```

`fdb_spawn` takes in a Rust Future and polls it until Ready.
```rs
pub fn fdb_spawn<F>(f: F)
where
    F: Future<Output = ()> + 'static,
{
    // Pins the Future in memory, this is mandatory for the Future to execute properly
    // The UnsafeCell is used for interior mutability
    let f = UnsafeCell::new(Box::pin(f));
    // The Future is wrapped into an FDBWaker and ref counted by an Arc
    let waker_arc = Arc::new(FDBWaker { f });
    // We build a RawWaker containing both the Arc<FdbWaker> as data and a pointer to VTABLE
    // VTABLE contains a custom implementation of a Waker
    let raw_waker = RawWaker::new(Arc::into_raw(waker_arc) as *const (), &VTABLE);
    // Finally we build a Waker from the RawWaker
    // Waker is only a wrapper around a RawWaker which maps the function of the Waker trait to
    // the RawWaker's vtable
    let waker = unsafe { Waker::from_raw(raw_waker) };
    // Here calling wake is stricly equivalent to any of:
    // - waker.inner.vtable.wake(waker.inner.data)
    // - raw_waker.vtable.wake(raw_waker.data)
    // - VTABLE[1](Arc::into_raw(waker_arc) as *const ())
    // - fdbwaker_wake(Arc::into_raw(waker_arc) as *const (), true)
    waker.wake();
    // waker is dropped here
}
```

`wake()` calls `fdbwaker_wake` (through `VTABLE[1]`) with `decrease` set to `true`.
> note: waker_ref reference the `Arc<FDBWaker>`, not directly `FDBWaker`
```rs
fn fdbwaker_wake(waker_ref: &FDBWaker, decrease: bool) {
    println!("wake {}", decrease);
    // This increases the ref counter of the Arc and gives back a RawWaker
    let waker_raw = fdbwaker_clone(waker_ref);
    // We build a Waker from the RawWaker
    let waker = unsafe { Waker::from_raw(waker_raw) };
    // We build a Context from the Waker
    // Context is only a wrapper around the Waker
    let mut cx = Context::from_waker(&waker);
    // We get the Future stored by the FDBWaker to poll it and discard the result
    let f = unsafe { &mut *waker_ref.f.get() };
    match f.as_mut().poll(&mut cx) {
        std::task::Poll::Ready(_) => println!("READY"),
        std::task::Poll::Pending => println!("PENDING"),
    }
    if decrease {
        // This decreases the ref counter of the Arc
        fdbwaker_drop(waker_ref);
    }
}
```

`poll()` runs the closure until encountering an `await`. `await` calls `poll` on its future and so on.
The case that we are interested in takes place when the foundationdb-rs issues a call to the fdbserver
using the C-API through `fdb_sys`. Calls that return a `FdbFutureHandle` are wrapped into `FdbFuture`
which implement poll in the following manner:
```rs
pub(crate) struct FdbFuture<T> {
    f: Option<FdbFutureHandle>,
    waker: Option<Arc<AtomicWaker>>,
    phantom: std::marker::PhantomData<T>,
}

impl<T> Future for FdbFuture<T>
where
    T: TryFrom<FdbFutureHandle, Error = FdbError> + Unpin,
{
    type Output = FdbResult<T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<FdbResult<T>> {
        println!("  POLL");
        let f = self.f.as_ref().expect("cannot poll after resolve");
        let ready = unsafe { fdb_sys::fdb_future_is_ready(f.as_ptr()) };
        if ready == 0 {
            // If the Future is not ready yet we try to get the AtomicWaker of the Future
            // If the AtomicWaker wasn't already set (if it is the first time poll() is called on
            // the Future) a new one is created (and ref counted with an Arc) and register is set
            // to true
            let f_ptr = f.as_ptr();
            let mut register = false;
            let waker = self.waker.get_or_insert_with(|| {
                register = true;
                Arc::new(AtomicWaker::new())
            });
            // Recall that Context is only a wrapper around a Waker
            // Here `cx` contains our FDBWaker
            /*
            Context {
                waker: Waker {
                    waker: RawWaker {
                        data: *const Arc<FDBWaker> as *const (),
                        vtable: &'static RawWakerVTable,
                    }
                }
            }
            */
            // register clones the Waker, which calls fdbwaker_clone through the vtable of the
            // RawWaker and atomically sets it as its latest waker
            waker.register(cx.waker());
            if register {
                println!("  REGISTER");
                // If it is the first time poll() is called on the Future
                // fdb_sys::fdb_future_set_callback is called to register a callback for this
                // FdbFutureHandle the second argument is a pointer that will be passed as argument
                // to the callback when called
                let network_waker: Arc<AtomicWaker> = waker.clone();
                let network_waker_ptr = Arc::into_raw(network_waker);
                unsafe {
                    fdb_sys::fdb_future_set_callback(
                        f_ptr,
                        Some(fdb_future_callback),
                        network_waker_ptr as *mut _,
                    );
                }
            }
            println!("  PENDING");
            Poll::Pending
        } else {
            println!("  READY");
            // If the Future is ready its result is querried and propagated
            Poll::Ready(
                error::eval(unsafe { fdb_sys::fdb_future_get_error(f.as_ptr()) })
                    .and_then(|()| T::try_from(self.f.take().expect("self.f.is_some()"))),
            )
        }
    }
}
```

To sum up what happened until now:
- `fdb_spawn` builds a ref counted `FDBWaker`, wraps it in a Waker and calls wake on it
- `fdbwaker_wake` was called with `decrease=true`, it increases the `FDBWaker` ref count and polls its Future
- in case an `FdbFuture` is polled, it:
  - registers our `Waker` into an `AtomicWaker` (cloning it in the process)
  - set an `fdb_future_callback` as callback and the `AtomicWaker` as argument for the `FdbFutureHandle`
  - returns `Poll::Pending`
- `fdbwaker_wake` decreases the ref count (because `decrease==true`) and returns 
- `fdb_spawn` drops the `Waker`, decreasing the ref count

```
wake true   // fdbwaker_wake
clone       // fdbwaker_wake
  POLL      // FdbFuture::Poll
clone       // FdbFuture::Poll
  REGISTER  // FdbFuture::Poll
  PENDING   // FdbFuture::Poll
PENDING     // fdbwaker_wake
drop        // fdbwaker_wake
drop        // fdb_spawn
```

After this, the Future was handed to the simulation and `fdb_spawn` returns. If your Workload is
valid, execution should be returned to the simulation. The fdbserver runs, internally resolving the
Future and, when done, notifies `FdbFutureHandle` by calling its callback with its argument.

```rs
extern "C" fn fdb_future_callback(
    _f: *mut fdb_sys::FDBFuture,
    callback_parameter: *mut ::std::os::raw::c_void,
) {
    println!("  CALLBACK");
    // The AtomicWaker is reconstructed from the callback_parameter
    let network_waker: Arc<AtomicWaker> = unsafe { Arc::from_raw(callback_parameter as *const _) };
    // The last Waker registered is woken up
    if let Some(waker) = network_waker.take() {
        waker.wake();
        // waker is dropped here
    }
}
```

The last `Waker` was registered by:
```rs
waker.register(cx.waker());
```
so in this case the `AtomicWaker` wakes our `Waker`, which calls `fdbwaker_wake` with `decrease=true`
which increases the ref count of `FDBWorker` and polls its Future.
This time `fdb_sys::fdb_future_is_ready` should return a non-zero value indicating the `FdbFuture`
was resolved by the fdbserver. So the `Waker` is not registered, and thus not cloned.
`FdbFuture::poll` returns `Poll::Ready`, and the closure continues to execute code until it reaches
a new `await`. In case it encounters a new `FdbFuture` the cycle continues exactly in the same way:
- `FdbFuture::poll` is called which:
  - registers the `Waker`, increasing the ref count
  - fdb_sys::fdb_future_set_callback is called with `fdb_future_callback` and the `AtomicWaker`
  - returns `Poll::Pending`
- `fdbwaker_wake` decreases the ref count (because `decrease==true`) and returns
- `fdb_future_callback` drops the `Waker` which decreases the ref count

```
wake true   // fdbwaker_wake
clone       // fdbwaker_wake
  POLL      // FdbFuture::Poll
  READY     // FdbFuture::Poll => a new FdbFuture is polled
  POLL      // FdbFuture::Poll
clone       // FdbFuture::Poll
  REGISTER  // FdbFuture::Poll
  PENDING   // FdbFuture::Poll
PENDING     // fdbwaker_wake
drop        // fdbwaker_wake
drop        // fdb_future_callback
```

On the other hand if the `FDBWaker`'s closure doesn't set a new callback in the simulation and
finishes it returns `Poll::Ready(())`.
The `Waker` was not registered and thus not cloned either.
The ref count decreases 2 times and reaches 0, freeing `FDBWaker` and its Pinned Future.
```
wake true   // fdbwaker_wake
clone       // fdbwaker_wake
  POLL      // FdbFuture::Poll
  READY     // FdbFuture::Poll => no FdbFuture to poll anymore
READY       // FdbFuture::Poll
drop        // fdbwaker_wake
drop        // fdb_future_callback
DROPPED     // FDBWaker::drop
```
