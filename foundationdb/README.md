# FoundationDB Rust Client API

This is a wrapper library around the FoundationDB (Fdb) C API. It implements futures based interfaces over the Fdb future C implementations.

## Prerequisites

* Rust 1.71.1 or more,
* FoundationDB's client installed.

## Platform Support

Support for different platforms ("targets") are organized into three tiers, each with a different set of guarantees. For more information on the policies for targets at each tier, see the [Target Tier Policy](#target-tier-policy).

| Platform       | Tier   | Notes                                                                                                                               |
|----------------|--------|-------------------------------------------------------------------------------------------------------------------------------------|
| linux x86_64   | 1      |                                                                                                                                     |
| osx x86_64     | 2      |                                                                                                                                     |
| Windows x86_64 | 3      | [Windows build has been officially discontinue, now maintained by the community](https://github.com/apple/foundationdb/issues/5135) |
| osx Silicon 	  | 3    	 | [Waiting for official dylib support](https://forums.foundationdb.org/t/arm-client-library/3072)                                     |

For more information on the policies for targets at each tier, see the

## Target Tier Policy

### Tier 1

`Tier 1` targets can be thought of as "guaranteed to work". This means that:

* we are actively checking correctness with the [BindingTester](https://github.com/apple/foundationdb/blob/master/bindings/bindingtester/spec/bindingApiTester.md),
* we are running classic Rust tests on each pull requests,
* you can use the crate on the platform.


### Tier 2

`Tier 2` targets can be thought of as "guaranteed to build". This means that:

* we are running classic Rust tests on each pull requests,
* you can use the crate on the platform.

But we are not checking correctness.

### Tier 3

`Tier 3` targets are platforms we would like to have as Tier 2. You might be able to compile, but no CI has been set up.

## Getting Started

### Install FoundationDB

You first need to install FoundationDB. You can follow the official documentation:

* [Getting Started on Linux](https://apple.github.io/foundationdb/getting-started-linux.html)
* [Getting started on macOS](https://apple.github.io/foundationdb/getting-started-mac.html)

### Add dependencies on foundationdb-rs

```shell
cargo add foundationdb -F embedded-fdb-include
cargo add futures
```

This Rust crate is not tied to any Async Runtime.

### Exposed features

| Features               | Notes                                                                          |
|------------------------|--------------------------------------------------------------------------------|
| `fdb-5_1`              | Support for FoundationDB 5.1.X                                                 |
| `fdb-5_2`              | Support for FoundationDB 5.2.X                                                 |
| `fdb-6_0`              | Support for FoundationDB 6.0.X                                                 |
| `fdb-6_1`              | Support for FoundationDB 6.1.X                                                 |
| `fdb-6_2`              | Support for FoundationDB 6.2.X                                                 |
| `fdb-6_3`              | Support for FoundationDB 6.3.X                                                 |
| `fdb-7_0`              | Support for FoundationDB 7.0.X                                                 |
| `fdb-7_1`              | Support for FoundationDB 7.1.X                                                 |
| `embedded-fdb-include` | Use the locally embedded FoundationDB fdb_c.h and fdb.options files to compile |
| `uuid`                 | Support for the uuid crate for Tuples                                          |
| `num-bigint`           | Support for the bigint crate for Tuples                                        |
| `tenant-experimental`  | Experimental support for tenants. Require at least 7.1                         |

### Hello, World using the crate

We are going to use the Tokio runtime for this example:

```rust
use futures::prelude::*;

#[tokio::main]
async fn main() {
    let network = unsafe {foundationdb::boot()};

    // Have fun with the FDB API
    hello_world().await.expect("could not run the hello world");

    // shutdown the client
    drop(network);
}

async fn hello_world() -> foundationdb::FdbResult<()> {
    let db = foundationdb::Database::default()?;

    // write a value in a retryable closure
    match db
        .run(|trx, _maybe_committed| async move {
            trx.set(b"hello", b"world");
            Ok(())
        })
        .await
    {
        Ok(_) => println!("transaction committed"),
        Err(_) => eprintln!("cannot commit transaction"),
    };

    // read a value
    match db
        .run(|trx, _maybe_committed| async move { Ok(trx.get(b"hello", false).await.unwrap()) })
        .await
    {
        Ok(slice) => assert_eq!(b"world", slice.unwrap().as_ref()),
        Err(_) => eprintln!("cannot commit transaction"),
    }

    Ok(())
}
```

## Additional notes

### The class-scheduling tutorial 

The official FoundationDB's tutorial is called the [Class Scheduling](https://apple.github.io/foundationdb/class-scheduling.html). You can find the Rust version in the [examples](https://github.com/foundationdb-rs/foundationdb-rs/tree/main/foundationdb/examples).

### The blob tutorial

The official FoundationDB documentation provides also [another topic](https://apple.github.io/foundationdb/largeval.html#modeling-large-values) 
which is further discussed inside a [design recipe](https://apple.github.io/foundationdb/blob.html). 
A Rust implementation can be found [here](https://github.com/foundationdb-rs/foundationdb-rs/tree/main/foundationdb/examples/blob.rs). 

Another [example](https://github.com/foundationdb-rs/foundationdb-rs/tree/main/foundationdb/examples/blob-with-manifest.rs), 
explores how to use subspaces to attach metadata to our blob.

### Must-read documentations

* [Developer Guide](https://apple.github.io/foundationdb/developer-guide.html)
* [Data Modeling Guide](https://apple.github.io/foundationdb/data-modeling.html)

### Initialization

Due to limitations in the C API, the Client and it's associated Network can only be initialized and run once per the life of a process. Generally the `foundationdb::boot` function will be enough to initialize the Client. See `foundationdb::api` for more configuration options of the Fdb Client.

###  Migration from 0.4 to 0.5

The initialization of foundationdb API has changed due to undefined behavior being possible with only safe code (issues #170, #181, pulls #179, #182).

Previously you had to wrote `foundationdb::boot().expect("failed to initialize Fdb");`, now this can be converted to:

```rust
// Safe because drop is called before the program exits
let network = unsafe { foundationdb::boot() };

// do stuff

// cleanly shutdown the client
drop(network);
```

### API stability

_WARNING_ Until the 1.0 release of this library, the API may be in constant flux.
