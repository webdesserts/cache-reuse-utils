#!/bin/sh
# Optional maintainer check. The checked-in header is used by normal builds.
set -eu
REPO=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
CBINDGEN=${CBINDGEN:-cbindgen}
"$CBINDGEN" --version | grep -q '^cbindgen 0\.29\.4$' || {
    echo "cbindgen 0.29.4 is required (set CBINDGEN)" >&2
    exit 2
}
TMP=$(mktemp "${TMPDIR:-/tmp}/cache-reuse-utils-header.XXXXXX")
trap 'rm -f "$TMP"' EXIT
"$CBINDGEN" --lang C --style type --config "$REPO/ffi/cbindgen.toml" \
    --output "$TMP" "$REPO/ffi/src/lib.rs"
cmp -s "$TMP" "$REPO/ffi/include/cache_reuse_utils.h" || {
    echo "cache_reuse_utils.h is stale; regenerate to a temp path" >&2
    exit 1
}
echo "cache_reuse_utils.h is current"
