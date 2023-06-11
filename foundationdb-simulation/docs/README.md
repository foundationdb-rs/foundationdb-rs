# foundationdb-simulation
The goal of this crate is to enable testing of rust layers in the official FoundationDB simulation.

## How does it work
FoundationDB is written in flow and transpiled to C++. Rust and C++ objects are not compatible so
every methods of all supported objects have been translated to a C function that takes a raw
pointer to the object as first argument.

This crate contains 4 types of wrappers:

- C++ that maps behavior to C bindings and will be called by the fdbserver directly
- Rust that implements C bindings from C++ (C++ to Rust bridge)
- Rust that maps behavior to C bindings
- C++ that implements C bindings from Rust (Rust to C++ bridge)

They are respectively contained in the files:
- `src/FDBWorkload.cpp`
- `src/lib.rs`
- `src/fdb_wrapper.rs`
- `src/FDBWrapper.cpp`

## Setup
Create a new Rust project following the library file structure:

```console
├── Cargo.toml
└── src/
    └── lib.rs
```

Add the foundationdb-workloads crate in your `Cargo.toml` dependencies section.
Write a lib section as follow:

```toml
[lib]
name = "myworkload"
crate-type = ["cdylib"]
```

It is necessary that the crate-type is set to `cdylib` as the FoundationDB simulation expects a
shared object. You can replace `myworkload` by the name of your workload.

## Workload
We abstracted the FoundationDB workloads with the following trait:

```rust
pub trait RustWorkload {
    fn description(&self) -> String;
    fn setup(&'static mut self, db: SimDatabase, done: Promise);
    fn start(&'static mut self, db: SimDatabase, done: Promise);
    fn check(&'static mut self, db: SimDatabase, done: Promise);
    fn get_metrics(&self) -> Vec<Metric>;
    fn get_check_timeout(&self) -> f64;
}
```

Define a struct and implement the `RustWorkload` trait on it. You can put anything in this struct,
it doesn't need to be FFI safe. We recommend you at least store the `WorkloadContext`.

Basic example:

```rust
struct MyWorkload {
    name: String,
    description: String,
    context: WorkloadContext,
}

impl MyWorkload {
    fn new(name: &str, context: WorkloadContext) -> Self {
        let name = name.to_string();
        let description = format!("Description of workload {:?}", name);
        Self {
            name,
            description,
            context,
        }
    }
}

impl RustWorkload for MyWorkload {
    fn description(&self) -> String {
        self.description.clone()
    }
    fn setup(&'static mut self, db: SimDatabase, done: Promise) {
        done.send(true);
    }
    fn start(&'static mut self, db: SimDatabase, done: Promise) {
        done.send(true);
    }
    fn check(&'static mut self, db: SimDatabase, done: Promise) {
        done.send(true);
    }
    fn get_metrics(&self) -> Vec<Metric> {
        Vec::new()
    }
    fn get_check_timeout(&self) -> f64 {
        3000.0
    }
```

## Entrypoint
Create a function with the name of your choice but with this exact signature:

```rust
pub fn main(name: &str, context: WorkloadContext) -> Box<dyn RustWorkload>;
```

Instantiate your workload in this function and return it. Add the `simulation_entrypoint`
proc macro and your workload is now registered in the simulation!

```rust
#[simulation_entrypoint]
pub fn main(name: &str, context: WorkloadContext) -> Box<dyn RustWorkload> {
    Box::new(MyWorkload::new(name, context))
}
```

In the simulation configuration, workloads have a `workloadName`. This string will be passed to
your entrypoint as first argument. It was designed so that you can implement several workloads in
a single library and chose which one to use directly in the configuration file without recompiling.

Basic example:

```rust
#[simulation_entrypoint]
pub fn main(name: &str, context: WorkloadContext) -> Box<dyn RustWorkload> {
    match name {
        "MyWorkload1" => Box::new(MyWorkload1::new(name, context)),
        "MyWorkload2" => Box::new(MyWorkload2::new(name, context)),
        "MyWorkload3" => Box::new(MyWorkload3::new(name, context)),
        name => panic!("no workload with name: {:?}", name),
    }
}
```

> /!\ You must have one and only one entrypoint in your project.

## Compilation
If you followed those steps you should be able to compile your workload using the standard build
command of cargo (`cargo build` or `cargo build --release`). This should create a shared object
file in `./target/debug/` or `./target/release/` named with the `name` you set in the `lib` section
of your `Cargo.toml` file, with a `.so` extension and prefixed with "lib". In this example we named
the lib `myworkload`, so the shared object file would be named `libmyworkload.so`.

## Launch
The foundationdb simulator takes a toml file as input

```console
fdbserver -r simulation -f ./test_file.toml
```

which describes the simulation to run. A simulation can contain several workloads (see the official
[documentation](https://apple.github.io/foundationdb/client-testing.html#write-the-test)
for this part). A RustWorkload should be loaded as an
[ExternalWorkload](https://github.com/apple/foundationdb/blob/main/fdbserver/workloads/ExternalWorkload.actor.cpp)
by specifying `testName=External`. `libraryPath` and `libraryName` must point to your shared object:

```toml
testTitle=MyTest
  testName=External
  workloadName=MyWorkload
  libraryPath=./target/debug/
  libraryName=myworkload
  myCustomOption=42
```


# API
In addition of the `RustWorkload` trait, here are all the enumerations, macros, structures and
methods you have access to in this crate:

```rust
enum Severity {
    Debug,
    Info,
    Warn,
    WarnAlways,
    Error,
}

struct WorkloadContext {
    fn trace<S>(&self, sev: Severity, name: S, details: Vec<(String, String)>);
    fn get_process_id(&self) -> u64;
    fn set_process_id(&self);
    fn now(&self) -> f64;
    fn rnd(&self) -> u32;
    fn get_option<T>(&self, name: &str) -> Option<T>;
    fn client_id(&self) -> usize;
    fn client_count(&self) -> usize;
    fn shared_random_number(&self) -> u64;
}

struct Metric {
    fn avg<S>(name: S, value: f64);
    fn val<S>(name: S, value: f64);
}

struct Promise {
    fn send(&mut self, val: bool);
}

fn fdb_spawn<F>(future: F);

type Details = Vec<String, String>;

macro details;
macro simulation_entrypoint;
```

The crate also exports the function `CPPWorkloadFactory` which you should not use!

## Trace
You can use `WorkloadContext::trace` to add log entries in the fdbserver logging file.

Example:

```rust
fn setup(&'static mut self, db: SimDatabase, done: Promise) {
    self.context.trace(
        Severity::Info,
        "Successfully setup workload",
        details![
            "name" => self.name,
            "description" => self.description(),
        ],
    );
    done.send(true);
}
```

> note: any log with a severity of `Severity::Error` will automatically stop the fdbserver

## Random
`WorkloadContext::rnd` and `WorkloadContext::shared_random_number` can be used to get or initialize
determinist random processus inside your workload.

## Get option
In the simulation configuration file you can add custom parameters to your workload.
These parameters can be read with `WorkloadContext::get_option`. This method will first try to get
the parameter value as a raw string and then convert it in a the type of your choice.
If the parameter doesn't exist, its value is invalid or set to `null`, the function returns `None`.

Example:

```rust
fn init(&mut self, context: WorkloadContext) -> bool {
    let count: usize = self
        .context
        .get_option("myCustomOption")
        .unwrap();
    true
}
```

> note: you **have** to consume any parameter you set in the config file.
> If you do not read a parameter the fdbserver will trigger an error.

# Lifecycle

## Instantiation
When the fdbserver is ready, it will load your shared object and try to instantiate a workload
from it. It is at that time that your entrypoint is called. The simulation creates a random number
of "clients" and each one runs a workload. Your entrypoint will be called as many times as there
is clients.

> note: contrary to the `ExternalWorkload` which has a separate `create` and `init` method, the
> `RustWorkloads` is not created until the "init" phase.

## Setup/Start/Check
Those 3 phases are run in order for all workloads. All workloads have to finish one phase for the
next one to start. Those phases are run asynchronously in the simulator and a workload indicates
it has finish by sending a boolean in its `done` promise.

An important thing to understand is that **any code you write is blocking**. To ensure that the 
simulation is determinist and repeatable, any time your code is running the fdbserver waits.
In other words, you **have** to hand the execution over to the fdbserver for anything to happen
on the database. To make it clear, this won't work:

```rust
fn setup(&'static mut self, db: SimDatabase, done: Promise) {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let trx = db.create_trx().unwrap();
            let version1 = trx.get_read_version().await.unwrap();
            println!("version1: {}", version1);
            let version2 = trx.get_read_version().await.unwrap();
            println!("version2: {}", version2);
        });
    done.send(true);
}
```

This code is blocking, it creates a transaction and tries to commit but nothing on the database
can happen until `setup` returns. This is a deadlock, `trx.commit().await` waits for fdbserver
to continue, and the fdbserver waits for `setup` to end to execute any action pending on the
database. FoundationDB works with callbacks. The only thing you can do in those asynchronous
sections is to set a callback and let the function end. The fdbserver then kicks back in, and calls
your callback when it's ready. Uppon entering your callback, fdbserer stops again and waits for the
callback to finish. In it you can set another callback or send a boolean to the `done` promise.

This may look like this:

```rust
use foundationdb_sys::*;

fn setup(&'static mut self, db: Database, done: Promise) {
    let trx = db.create_trx()
    let f = fdb_transaction_get_read_version(trx);
    fdb_future_set_callback(f, callback1, CallbackData { trx, done });
}

fn callback1(f: *mut FDBFuture, data: CallbackData) {
    let mut version1;
    fdb_future_get_int64(f, &mut version1);
    println!("version1: {}", version1);
    let f = fdb_transaction_get_read_version(data.trx);
    fdb_future_set_callback(f, callback2, data);
}

fn callback2(f: *mut FDBFuture, data: CallbackData) {
    let mut version2;
    fdb_future_get_int64(f, &mut version2);
    println!("version2: {}", version1);
    data.done.send(true);
}
```

This is really cumbersome and errorprone to write. This is only correct way to communicate between
the workload and the fdbserver that:
- works (no deadlock, no invalid pointers...)
- ensure determinism

Using other standard runtimes from tokio or other libraries doesn't work and using different
threads would break determinism and isn't supported. A "wild" thread would be detected by the
fdbserver and crash. You would have to register it but we didn't implemented the bindings to
enable this. However we implemented a custom executor that simplifies a lot how it's written,
but does exactly the same thing under the hood. This example would be written:

```rust
fn setup(&'static mut self, db: SimDatabase, done: Promise) {
    fdb_spawn(async {
        let trx = db.create_trx().unwrap();
        let version1 = trx.get_read_version().await.unwrap();
        println!("version1: {}", version1);
        let version2 = trx.get_read_version().await.unwrap();
        println!("version2: {}", version2);
        done.send(true);
    });
}
```

`fdb_spawn` is a naive future executor that uses the fdbserver as reactor. It is only compatible
with futures that set callbacks in the fdbserver. So you can't use any async code in it. Any future
that isn't created by foundationdb-rs is subject to deadlock. All this is highly experimental so we
highly appreciate any feedback on it (alternatives, ameliorations, errors...).

### Common mistakes
The `done` promise has to be used. If you don't, the fdbserver crashes and you should see in the
log file a line saying `BrokenPromise`. This is expected behavior, explicitely tracked by fdbserver
and implemented on purpose by our wrapper. This is to prevent a deadlock, as a workload that does
not resolve its promise is considered as never ending and block the execution of all remaining
phases without triggering any error.

On the contrary, setting the value of `done` more than once is also an error. Doing so will
terminate the workload by panicking.

> note: `Promise::send` consumes the `Promise` to prevent it from being resolved twice.

Sending `false` in `done` doesn't trigger any error. In fact sending `true` or `false` is strictly
equivalent for the fdbserver. The only thing that counts is that `done` has a been resolved.

Indirectly using a pointer to the workload or to the database after resolving `done` is undefined
behavior. Resolving `done` should be the very last thing you do in a phase, it indicates to the
fdbserver that you are finished and many structures may be relocated in memory, so you no longer
have any garantee on the validity of any object on the Rust side. You must wait for the next phase
and use the new references to `self` and `db` you are given. For this reason don't try to store 
`RustWorkload`, `SimDatabase`, `Promise` instances or any object created through foundation-rs
bindings (transactions, futures...) as using them accross phases will most certainly result in a
segmentation fault.

## Metrics
At the end of the simulation `get_metrics` will be called and you have the possibility to return
a vector of `Metric`. Each metric can represent a raw value or an average.

Example:

```rust
fn get_metrics(&self) -> Vec<Metric> {
    vec![
        Metric::avg("foo", 42.0),
        Metric::val("bar", 418.0),
        Metric::val("baz", 1337.0),
    ]
}
```
