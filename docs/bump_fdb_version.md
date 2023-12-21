# How to bump FoundationDB version?

1. In `foundationdb-sys`:
   1. create the new feature in `Cargo.toml`
   2. update build.rs with the new feature
   3. Download the corresponding debian fdb client from Github and dpkg -x the deb file to retrieve files in usr/include/foundationdb
   4. put the files in include/{{ fdb_version }}
   5. add a version.txt file with the full release
