# foundationdb-simulation

This crate provides the tools to write Rust workloads that can be loaded and executed in the
official FoundationDB simulator, allowing for rigorous and deterministic testing of Rust layers.

## How It Works

FoundationDB's simulation framework includes an `ExternalWorkload` that can dynamically load a
shared object at runtime. This shared object must expose specific symbols that the simulator calls
to drive the workload.

Originally, `ExternalWorkload` used a C++ interface. This is cumbersome because the C++ ABI is not
stable and most languages do not interoperate with it easily. As of FoundationDB 7.4, the
`ExternalWorkload` supports a pure C interface, which this crate targets. For backwards
compatibility with FoundationDB 7.1 and 7.3, a C++ shim is automatically compiled to translate the
C interface back to the C++ one.

### For FoundationDB 7.4 and Newer (Recommended)

- Uses a pure C API (FFI-safe).
- **No need** to build inside the official FoundationDB Docker image.
- Requires setting `useCAPI=true` in the test configuration file.
- Results in faster build times and a simpler setup.

### For FoundationDB 7.1 and 7.3

- Requires a C++ shim to bridge the C and C++ ABIs.
- **Must** be built within the [official `foundationdb/build` Docker image](https://hub.docker.com/r/foundationdb/build)).
- The linker must be set to `clang`.
- Involves a more complex build process due to C++ ABI compatibility requirements.

## Setup

First, create a new Rust project with a library structure:

```console
.
├── Cargo.toml
└── src/
    └── lib.rs
```

Next, add `foundationdb-simulation` to your `Cargo.toml` and configure your crate as `cdylib`
as the `ExternalWorkload` expects a shared object.

```toml
[lib]
name = "myworkload"
crate-type = ["cdylib"]

[dependencies]
# Make sure to select the feature flag for your target FDB version.
foundationdb-simulation = { version = "...", features = ["fdb-7_4"] } # Or "fdb-7_1", "fdb-7_3"
```

## Compilation

If you are targeting FDB 7.1 or 7.3 (which require the C++ shim), you **must** compile your
workload inside the official FoundationDB Docker image. Compiling outside of this environment
will almost certainly lead to segmentation faults at runtime due to C++ ABI mismatches.

Compile your workload using the standard Cargo release profile:

```bash
cargo build --release
```

This will create a shared object in `./target/release/`. The filename will be `lib<name>.so`,
where `<name>` is the `name` you set in the `[lib]` section of your `Cargo.toml`.
For example: `libmyworkload.so`.

## Launching the Simulation

The FoundationDB simulator is launched via the `fdbserver` binary with a TOML configuration file.

```bash
fdbserver -r simulation -f ./test_file.toml
```

Your test file must specify `testName=External` to use the `ExternalWorkload` framework.

```toml
testTitle = "MyRustTest"
testName = "External"

# The name passed to your workload factory
workloadName = "MyWorkload" 

# The path to the directory containing your .so file
libraryPath = "./target/release/" 
# The name of your library from Cargo.toml (without lib/.so)
libraryName = "myworkload" 

# Required for FDB 7.4+ when using the C API
useCAPI = true

# Custom options for your workload
myCustomOption = 42
```

## Workload Definition

FoundationDB workloads are defined by implementing the `RustWorkload` trait:

```rust
pub trait RustWorkload {
    fn setup(&'static mut self, db: Database, done: Promise);
    fn start(&'static mut self, db: Database, done: Promise);
    fn check(&'static mut self, db: Database, done: Promise);
    fn get_metrics(&self) -> Vec<Metric>;
    fn get_check_timeout(&self) -> f64;
}
```

The simulator expects the shared object to expose a factory that can create workload instances.
This crate provides two traits to define these factories.

For simple use cases where a single workload implementation is defined, you can implement the
`SingleRustWorkload` trait.

```rust
pub trait SingleRustWorkload: RustWorkload {
    const FDB_API_VERSION: u32;
    fn new(name: String, context: WorkloadContext) -> Self;
}
```

For more complex scenarios, you can use the `RustWorkloadFactory` trait to instantiate different
`RustWorkload` types based on the workload name provided in the test configuration.

```rust
pub trait RustWorkloadFactory {
    const FDB_API_VERSION: u32;
    fn create(name: String, context: WorkloadContext) -> WrappedWorkload;
}
```

See the `atomic` and `noop` implementations in the `examples/` directory for complete working examples.

## The WorkloadContext

The `WorkloadContext` passed to the factory is the primary way to interact with the simulator.
It provides several useful methods:

- `trace(severity, name, details)`: Add a log entry to the FDB trace files.
- `now()`: Get the current simulated time as a `f64`.
- `rnd()`: Get a deterministic 32-bit random number.
- `shared_random_number()`: Get a deterministic 64-bit random number (the same for all clients).
- `client_id()`: Get the ID of the current client.
- `client_count()`: Get the total number of clients in the simulation.
- `get_option<T>(name)`: Get a custom configuration option from the test file.

### Tracing

Use `WorkloadContext::trace` to log messages with a given severity and a map of string "details".
A severity of `Severity::Error` will automatically stop the `fdbserver` process.

```rust
fn setup(&'static mut self, db: SimDatabase, done: Promise) {
    self.context.trace(
        Severity::Info,
        "SuccessfullySetupWorkload",
        details![
            "Layer" => "Rust",
            "Name" => self.name.clone(),
            "Phase" => "setup",
        ],
    );
    done.send(true);
}
```

### Randomness

To maintain determinism, all random numbers must be sourced from the simulator.
Use `WorkloadContext::rnd()` or `WorkloadContext::shared_random_number()` for a shared seed.
Do not use external entropy sources like `rand::thread_rng()`.

# Workload Lifecycle

## Instantiation

Once the `fdbserver` process is ready, it loads your shared object and calls your workload factory
to create instances. The simulator creates a random number of "clients," and your factory will be
called once for each client.

> **Note:** Contrary to the C++ `ExternalWorkload` which has separate `create` and `init` methods,
> the `RustWorkload` is not created until the simulator's `init` phase.

The `WorkloadContext` passed to your factory should be stored in your `RustWorkload`, as it is safe
to use across phases and will not be provided again.

## The setup, start, and check Phases

These three phases run sequentially. The simulation will not begin the `start` phase until all
clients have completed the `setup` phase, and so on. These phases are asynchronous from the
simulator's perspective. A workload signals that it has finished its current phase by resolving
the `done` promise.

It is critical to understand that **any Rust code you write is blocking**. To ensure the simulation
is deterministic, the `fdbserver` process pauses whenever your workload code is executing.
You **must** yield control back to the simulator for any database operations to occur.

For example, the following code will cause a **deadlock**:

```rust
fn setup(&'static mut self, db: SimDatabase, done: Promise) {
    // This creates a standard Rust async runtime, which is separate from
    // the FDB simulator's event loop.
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let trx = db.create_trx().unwrap();
            let version = trx.get_read_version().await.unwrap();
            println!("version: {}", version);
        });
    done.send(true);
}
```

This deadlocks because `get_read_version().await` waits for `fdbserver` to process the request,
but `fdbserver` is waiting for the `setup` function to return.

The correct approach is to use FoundationDB's callback-based futures. The raw `foundationdb_sys`
bindings are verbose and require manual callback management:

```rust
use foundationdb_sys::*;

fn setup(&'static mut self, db: Database, done: Promise) {
    let trx = db.create_trx();
    let f = fdb_transaction_get_read_version(trx);
    // Set a callback to be invoked by the FDB event loop later
    fdb_future_set_callback(f, on_version_ready, Box::into_raw(Box::new(MyCallbackData { trx, done })));
}

extern "C" fn on_version_ready(f: *mut FDBFuture, user_data: *mut c_void) {
    // Now we are inside a callback, so the fdbserver is waiting for us again.
    let data = unsafe { Box::from_raw(user_data as *mut MyCallbackData) };
    let mut version: i64 = 0;
    fdb_future_get_int64(f, &mut version);
    println!("version: {}", version);
    // We are done.
    data.done.send(true);
}
```

To simplify this, `foundationdb-simulation` provides a small, native async executor that integrates
with the FDB event loop. You can use familiar `async/await` syntax, but
**only with futures produced by the `foundationdb-rs` crate**.

This is how the same logic should be written:

```rust
fn setup(&'static mut self, db: SimDatabase, done: Promise) {
    // fdb_spawn schedules the async block to be driven by the FDB event loop.
    fdb_spawn(async move {
        let trx = db.create_trx().unwrap();
        let version = trx.get_read_version().await.unwrap();
        println!("version: {}", version);
        done.send(true);
    });
}
```

`fdb_spawn` uses the `fdbserver` itself as the reactor. Attempting to `.await` any future not
created by `foundationdb-rs` may cause a deadlock. This feature is experimental, and we welcome
feedback, bug reports, and suggestions.

## Reporting Metrics

At the end of the simulation, the `get_metrics` method is called for each client.
Implement this method to report results.

```rust
fn get_metrics(&self, out: Metrics) {
    // val metrics are summed across all clients
    out.push(Metric::val("total_ops", self.ops as f64));
    // avg metrics are averaged across all clients
    out.push(Metric::avg("avg_latency", self.latency / self.ops.max(1) as f64));
}
```

# Common Mistakes

* **Compiling C++ shim outside of the Docker.** C++ ABI is extremely environment-dependent, not
  compiling in the exact same environment as the oter half of the interface will probably result
  in segmentation faults or mangled strings that will trigger unrecoverable errors.

* **Forgetting to read all options.** Any custom option in the configuration that is not read will
  trigger a `Workload had invalid options.` error.

* **Forgetting useCAPI=true.** If the `fdbserver` used supports the C API, you still need to add
  explicitely this option, otherwise the `ExternalWorkload` will try to load the C++ symbol. This
  error can be confusing since it won't print a specific error message, instead it will complain
  about unrecognized options. In the log files you should find `undefined symbol: WorkloadFactory`.

* **Forgetting to resolve the `done` promise.** If you do not call `done.send(...)` at the end
  of a phase, this will trigger a `BrokenPromise` error if the `done` is dropped, and hang if it
  is somehow kept alive.

* **Resolving the `done` promise more than once.** This will cause the workload to panic immediately.
  The API helps prevent this, as `Promise::send` consumes `self`.

* **Thinking `done.send(false)` signals an error.** For the simulator, sending `true` or `false` is
  equivalent. All that matters is that the promise is resolved. Workload failure should be signaled
  via `Severity::Error` traces or by panicking.

* **Using pointers or references after a phase ends.** Resolving the `done` promise should be the
  final action in a phase. Once it's resolved, the simulator may move or deallocate memory. Any
  pointers or references to FDB objects become invalid. You must use the fresh `&'static mut self`
  and `db` references passed to the next phase. Storing `Database`, `Transaction`, or `Future`
  objects across phases will lead to segmentation faults or other undefined behavior.
