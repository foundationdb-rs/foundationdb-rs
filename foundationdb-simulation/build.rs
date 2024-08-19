extern crate cc;

fn main() {
    let api_version;

    #[cfg(feature = "fdb-7_1")]
    {
        api_version = String::from("710");
    }

    #[cfg(feature = "fdb-7_3")]
    {
        api_version = String::from("730");
    }

    #[cfg(feature = "fdb-7_3")]
    {
        // FDB 7.3 is built with clang, so we need to do the same, including the linker
        #[cfg(feature = "fdb-docker")]
        build_with_clang(&api_version);

        // We still needs to support building something wrong from FDB's perspective but valid in Rust,
        // to publish the crate and the doc
        #[cfg(not(feature = "fdb-docker"))]
        {
            display_build_warnings();
            crate::build_with_gcc(&api_version);
        }
    }

    #[cfg(feature = "fdb-7_1")]
    {
        #[cfg(not(feature = "fdb-docker"))]
        display_build_warnings();

        // FDB is built with gcc
        build_with_gcc(&api_version);
    }
}

fn display_build_warnings() {
    println!("cargo:warning=---------------------");
    println!("cargo:warning=------ Warning ------");
    println!("cargo:warning=---------------------");
    println!("cargo:warning=Building the crate without the `fdb-docker` feature will be valid from Rust's perspective but not from C++ and FoundationDB");
    println!("cargo:warning=Please follow the instructions in the associated README");
}

fn build_with_gcc(api_version: &str) {
    cc::Build::new()
        .cpp(true)
        .std("c++14")
        .define("FDB_API_VERSION", api_version)
        .file("src/FDBWrapper.cpp")
        .file("src/FDBWorkload.cpp")
        .compile("libctx.a");
}

fn build_with_clang(api_version: &str) {
    cc::Build::new()
        .compiler("clang")
        .cpp_set_stdlib("c++")
        .cpp(true)
        .std("c++14")
        .define("FDB_API_VERSION", api_version)
        .file("src/FDBWrapper.cpp")
        .file("src/FDBWorkload.cpp")
        .compile("libctx.a");
}
