# FoundationDB Rust Client API

This is a wrapper library around the FoundationDB (Fdb) C API. It implements futures based interfaces over the Fdb future C implementations.

## Prerequisites

Rust 1.46+

### Install FoundationDB

Install FoundationDB on your system, see [FoundationDB Local Development](https://apple.github.io/foundationdb/local-dev.html), or these instructions:

- Ubuntu Linux (this may work on the Linux subsystem for Windows as well)

```console
$> curl -O https://www.foundationdb.org/downloads/6.2.15/ubuntu/installers/foundationdb-clients_6.2.25-1_amd64.deb
$> curl -O https://www.foundationdb.org/downloads/6.2.15/ubuntu/installers/foundationdb-server_6.2.25-1_amd64.deb
$> sudo dpkg -i foundationdb-clients_6.2.25-1_amd64.deb
$> sudo dpkg -i foundationdb-server_6.2.25-1_amd64.deb
```

- macOS

```console
$> curl -O https://www.foundationdb.org/downloads/6.2.25/macOS/installers/FoundationDB-6.2.25.pkg
$> sudo installer -pkg FoundationDB-6.2.25.pkg -target /
```

- Windows

https://www.foundationdb.org/downloads/6.2.25/windows/installers/foundationdb-6.2.25-x64.msi

## Add dependencies on foundationdb-rs

```toml
[dependencies]
foundationdb = "0.5"
futures = "0.3"
```

## Initialization

Due to limitations in the C API, the Client and it's associated Network can only be initialized and run once per the life of a process. Generally the `foundationdb::boot` function will be enough to initialize the Client. See `foundationdb::api` for more configuration options of the Fdb Client.

## Example

```rust
use futures::prelude::*;

async fn async_main() -> foundationdb::FdbResult<()> {
    let db = foundationdb::Database::default()?;

    // write a value
    let trx = db.create_trx()?;
    trx.set(b"hello", b"world"); // errors will be returned in the future result
    trx.commit().await?;

    // read a value
    let trx = db.create_trx()?;
    let maybe_value = trx.get(b"hello", false).await?;
    let value = maybe_value.unwrap(); // unwrap the option

    assert_eq!(b"world", &value.as_ref());

    Ok(())
}

// Safe because drop is called before the program exits
let network = unsafe { foundationdb::boot() };
futures::executor::block_on(async_main()).expect("failed to run");
drop(network);
```

```rust
#[tokio::main]
async fn main() {
    // Safe because drop is called before the program exits
    let network = unsafe { foundationdb::boot() };

    // Have fun with the FDB API

    // shutdown the client
    drop(network);
}
```

## Migration from 0.4 to 0.5

The initialization of foundationdb API has changed due to undefined behavior being possible with only safe code (issues #170, #181, pulls #179, #182).

Previously you had to wrote:

```rust
let network = foundationdb::boot().expect("failed to initialize Fdb");

futures::executor::block_on(async_main()).expect("failed to run");
// cleanly shutdown the client
drop(network);
```

This can be converted to:

```rust
// Safe because drop is called before the program exits
let network = unsafe { foundationdb::boot() };

futures::executor::block_on(async_main()).expect("failed to run");

// cleanly shutdown the client
drop(network);
```

## API stability

_WARNING_ Until the 1.0 release of this library, the API may be in constant flux.
