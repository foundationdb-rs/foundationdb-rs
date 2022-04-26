#! /bin/bash -e

set -x

fdb_rs_dir=$(pwd)
bindingtester="${fdb_rs_dir:?}/$1"
case $(uname) in
  Darwin)
    brew install mono
  ;;
  Linux)
    sudo apt update
    sudo apt install mono-devel ninja-build liblz4-dev -y
  ;;
  *) echo "only macOS or Ubuntu is supported"
esac

## build the python bindings
(
  fdb_builddir=${fdb_rs_dir:?}/target/foundationdb_build
  mkdir -p ${fdb_builddir:?}
  cd ${fdb_builddir:?}

  ## Get foundationdb source
  git clone --depth 1 https://github.com/apple/foundationdb.git -b release-7.0
  cd foundationdb
  git checkout release-7.0

  ## build python api bindings
  mkdir cmake_build && cd cmake_build
  cmake -G Ninja ../
  ninja python_binding
  cp ./bindings/python/fdb/fdboptions.py ../bindings/python/fdb/fdboptions.py
  cd ..

  echo "testers['rust'] = Tester('rust', '${bindingtester}', 2040, 23, 700, types=ALL_TYPES)
" >> ./bindings/bindingtester/known_testers.py
)

cd "${fdb_rs_dir}";