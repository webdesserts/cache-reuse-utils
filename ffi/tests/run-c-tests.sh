#!/bin/sh
set -eu
REPO=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
TARGET_DIR=${1:-${CARGO_TARGET_DIR:-"$REPO/target"}}
case "$TARGET_DIR" in
    /*) ;;
    *) echo "target directory must be absolute: $TARGET_DIR" >&2; exit 2 ;;
esac
LIB="$TARGET_DIR/release/libcache_reuse_utils_ffi.a"
cargo build --release --locked --jobs 2 --target-dir "$TARGET_DIR" \
    --manifest-path "$REPO/ffi/Cargo.toml"
test -f "$LIB" || { echo "missing built static library: $LIB" >&2; exit 1; }
CC=${CC:-cc}
"$CC" -std=c11 -Wall -Wextra -I"$REPO/ffi/include" \
    "$REPO/ffi/tests/scr_planner_ffi_test.c" "$LIB" -lpthread -ldl -lm \
    -o "$REPO/ffi/tests/scr_planner_ffi_test"
"$CC" -std=c11 -Wall -Wextra -I"$REPO/ffi/include" \
    "$REPO/ffi/tests/abi.c" "$LIB" -lpthread -ldl -lm \
    -o "$REPO/ffi/tests/abi"
"$REPO/ffi/tests/scr_planner_ffi_test"
"$REPO/ffi/tests/abi"
