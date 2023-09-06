extern crate cc;

fn main() {
    cc::Build::new()
        .cpp(true)
        .define("FDB_API_VERSION", "710") // TODO: support older versions?
        .file("src/FDBWrapper.cpp")
        .file("src/FDBWorkload.cpp")
        .compile("libctx.a");
}
