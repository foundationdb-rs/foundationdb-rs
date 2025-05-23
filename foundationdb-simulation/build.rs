use std::{env, path::PathBuf};

extern crate cc;

fn main() {
    let bindings = bindgen::Builder::default()
        .header("src/headers/CWorkload.h")
        .generate()
        .expect("generate bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("write bindings");

    let _api_version: String;

    #[cfg(feature = "fdb-7_1")]
    {
        _api_version = String::from("710");
    }

    #[cfg(feature = "fdb-7_3")]
    {
        _api_version = String::from("730");
    }

    #[cfg(feature = "fdb-7_4")]
    {
        _api_version = String::from("740");
    }

    #[cfg(feature = "cpp-abi")]
    {
        // The Cpp API is not FFI safe so we need to compile in the exact same environment
        if cfg!(feature = "fdb-docker") {
            if cfg!(feature = "fdb-7_1") {
                build_with_gcc(&_api_version);
            } else {
                // FDB 7.3+ is built with clang, so we need to do the same, including the linker
                build_with_clang(&_api_version);
            }
        } else {
            // We still needs to support building something wrong from FDB's perspective but valid in Rust,
            // to publish the crate and the doc
            display_build_warnings();
            build_with_gcc(&_api_version);
        }
    }
}

#[allow(dead_code)]
fn display_build_warnings() {
    println!("cargo:warning=---------------------");
    println!("cargo:warning=------ Warning ------");
    println!("cargo:warning=---------------------");
    println!("cargo:warning=Building the C++ bindings without the `fdb-docker` feature will be valid from Rust's perspective but not from C++ and FoundationDB");
    println!("cargo:warning=Please follow the instructions in the associated README");
}

#[allow(dead_code)]
fn build_with_gcc(api_version: &str) {
    cc::Build::new()
        .cpp(true)
        .std("c++14")
        .define("FDB_API_VERSION", api_version)
        .file("src/CppWorkload.cpp")
        .compile("ctx");
}

#[allow(dead_code)]
fn build_with_clang(api_version: &str) {
    cc::Build::new()
        .compiler("clang")
        .cpp_set_stdlib("c++")
        .cpp(true)
        .std("c++14")
        .define("FDB_API_VERSION", api_version)
        .file("src/CppWorkload.cpp")
        .compile("ctx");
}
