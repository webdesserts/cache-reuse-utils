# Contributing

## Build and test

The core crate (`cache-reuse-utils`) uses only the Rust standard library (`std`),
with zero external dependencies and `#![forbid(unsafe_code)]`.

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
```

Prefer API-level integration tests under `tests/` that exercise the public
contract as a caller would use it. Module-private unit tests are appropriate for
focused algorithmic invariants (such as the 2D Fenwick-staircase index or hash
bucket bounds) that would be indirect to isolate through the public API.

## C ABI checks

The `ffi/` crate (`cache-reuse-utils-ffi`) is an explicit workspace member, while
the safe core crate remains the default member. Running `cargo test --workspace`
executes both the core and FFI Rust test suites.

To compile and run the standalone C test suite against the release static
library and checked-in header (`ffi/include/cache_reuse_utils.h`):

```sh
./ffi/tests/run-c-tests.sh /absolute/owned/target
```

`run-c-tests.sh` defaults to `<repo>/target`, passes that target directory to
both Cargo and the C compiler linker step, and writes compiled test binaries to
`<target>/c-tests`.

Although this workspace sets `panic = "unwind"` in `[profile.release]`, Cargo
always takes profile settings from the **root** workspace manifest. Any
downstream Rust workspace that links `cache-reuse-utils-ffi` and relies on its
FFI `catch_unwind` boundary must also set `[profile.release] panic = "unwind"`
in its own root `Cargo.toml`.

Checking header freshness against the Rust FFI definitions is optional and
requires `cbindgen` 0.29.4 (the script generates into a temporary file and diffs
against `ffi/include/cache_reuse_utils.h` without modifying the tracked header):

```sh
CBINDGEN=/path/to/cbindgen-0.29.4 ./tools/check-header.sh
```
