#!/usr/bin/env bash

set -ex;

for CRATE in foundationdb foundationdb-sys foundationdb-gen
do
    git-cliff --include-path "$CRATE/**" > "$CRATE/CHANGELOG.md"
done
