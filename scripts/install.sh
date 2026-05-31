#!/usr/bin/env sh
set -eu

prefix="${SIMX_INSTALL_PREFIX:-/usr/local}"
bindir="$prefix/bin"

cargo build --release
mkdir -p "$bindir"
cp target/release/simx "$bindir/simx"

echo "installed simx to $bindir/simx"
