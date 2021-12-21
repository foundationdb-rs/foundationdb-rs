#! /bin/bash -e

set -x

fdb_rs_dir=$(pwd)
fdb_builddir=${fdb_rs_dir:?}/target/foundationdb_build
cd "${fdb_builddir:?}/foundationdb"

START=1
END=${1-1}
for i in $(eval echo "{$START..$END}")
do
  echo "Running iteration $i"
  ./bindings/bindingtester/bindingtester.py --test-name scripted rust
  ./bindings/bindingtester/bindingtester.py --num-ops 1000 --api-version 630 --test-name api --compare python rust
  ./bindings/bindingtester/bindingtester.py --num-ops 1000 --api-version 630 --test-name api --concurrency 5 rust
done