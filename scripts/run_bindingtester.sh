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
    sudo apt install mono-devel -y
  ;;
  *) echo "only macOS or Ubuntu is supported"
esac

## build the python bindings
(
  fdb_builddir=${fdb_rs_dir:?}/target/foundationdb_build
  mkdir -p ${fdb_builddir:?}
  cd ${fdb_builddir:?}

  ## Get foundationdb source
  git clone --depth 1 https://github.com/apple/foundationdb.git -b release-6.1
  cd foundationdb
  git checkout release-6.1

  ## need the python api bindings
  make fdb_python

  ## Run the test
  echo "testers['rust'] = Tester('rust', '${bindingtester}', 2040, 23, MAX_API_VERSION, types=ALL_TYPES)
" >> ./bindings/bindingtester/known_testers.py
  python2 ./bindings/bindingtester/bindingtester.py --test-name scripted rust
  python2 ./bindings/bindingtester/bindingtester.py --num-ops 1000 --api-version 610 --test-name api --compare python rust
  python2 ./bindings/bindingtester/bindingtester.py --num-ops 1000 --api-version 610 --test-name api --concurrency 5 rust
)
