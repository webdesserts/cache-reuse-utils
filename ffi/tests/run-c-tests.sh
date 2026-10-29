#!/bin/sh
# Build the FFI static library and run the C ABI tests against it.
# Usage: run-c-tests.sh [absolute-target-dir]
set -eu
REPO=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
TARGET_DIR=${1:-${CARGO_TARGET_DIR:-"$REPO/target"}}
case "$TARGET_DIR" in
    /*) ;;
    *) echo "target directory must be absolute: $TARGET_DIR" >&2; exit 2 ;;
esac
LIB="$TARGET_DIR/release/libcache_reuse_utils_ffi.a"
OUT="$TARGET_DIR/c-tests"
cargo build --release --locked --jobs 2 --target-dir "$TARGET_DIR" \
    --manifest-path "$REPO/ffi/Cargo.toml"
test -f "$LIB" || { echo "missing built static library: $LIB" >&2; exit 1; }
mkdir -p "$OUT"
CC=${CC:-cc}
for test in cache_reuse_utils_ffi_test abi; do
    "$CC" -std=c11 -Wall -Wextra -Werror -I"$REPO/ffi/include" \
        "$REPO/ffi/tests/$test.c" "$LIB" -lpthread -ldl -lm \
        -o "$OUT/$test"
    "$OUT/$test"
done
