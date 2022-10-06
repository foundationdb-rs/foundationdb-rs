#!/usr/bin/env bash

set -ex;

fdb_api_version=710

fdb_rs_dir=$(pwd)
fdb_builddir=${fdb_rs_dir:?}/target/foundationdb_build
cd "${fdb_builddir:?}/foundationdb"

START=1
END=${1-1}
for i in $(eval echo "{$START..$END}")
do
  echo "Running iteration $i"
  ./bindings/bindingtester/bindingtester.py --num-ops 1000 --api-version $fdb_api_version --test-name api --compare python rust --seed 3181802154
done
