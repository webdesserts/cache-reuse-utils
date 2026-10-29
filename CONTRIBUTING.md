# Contributing

## Build and test

The core crate is `std`-only with no dependencies.

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
```

Prefer API-level integration tests under `tests/` that exercise the public
contract. Unit tests inside a module are appropriate for complex algorithm
internals that would be awkward to drive through the public API.

## C ABI checks

The FFI is an explicit workspace member, while the core remains the default
member. `cargo test --workspace` runs its Rust tests. The C tests
build the release archive and link C programs against the checked-in header:

```sh
./ffi/tests/run-c-tests.sh /absolute/owned/target
```

The C runner defaults to `<repo>/target`, uses the target directory argument
for both Cargo and the archive link, and writes its binaries to
`<target>/c-tests`. The workspace release profile is unwind, but a downstream
Cargo consumer's root profile still controls its Git dependency build; set
`[profile.release] panic = "unwind"` there when relying on the FFI's panic
containment. Header freshness is optional and requires cbindgen 0.29.4:

```sh
CBINDGEN=/path/to/cbindgen-0.29.4 tools/check-header.sh
```
