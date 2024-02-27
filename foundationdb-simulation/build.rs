extern crate cc;

fn main() {
    let api_version;

    #[cfg(feature = "fdb-7_1")]
    {
        api_version = 710;
    }
    #[cfg(feature = "fdb-7_3")]
    {
        api_version = 730;
    }

    let stringified_api_version = format!("{}", api_version);

    cc::Build::new()
        .cpp(true)
        .define("FDB_API_VERSION", stringified_api_version.as_str())
        .file("src/FDBWrapper.cpp")
        .file("src/FDBWorkload.cpp")
        .compile("libctx.a");
}
