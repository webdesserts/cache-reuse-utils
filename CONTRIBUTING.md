# Contributing

## Build and test

The default production crate is `std`-only. The opt-in `test-support` feature
adds the pinned `mock_instant` clock bridge for public integration tests; it is
never enabled by default.

```sh
cargo fmt --check
cargo check --offline
cargo test --offline                  # default production build; excludes clock bridge tests
cargo test --offline --features test-support
cargo clippy --offline --all-targets -- -D warnings
cargo clippy --offline --all-targets --features test-support -- -D warnings
```

The default suite omits `tests/test_support_clock.rs`; run the feature-enabled
suite to exercise the per-thread mock-clock bridge and public phase tests.

Prefer API-level integration tests under `tests/` that exercise the public
contract. Unit tests inside a module are appropriate for complex algorithm
internals that would be awkward to drive through the public API.

## C ABI checks

The FFI is an explicit workspace member, while the core remains the default
member:

```sh
cargo test --locked -p cache-reuse-utils-ffi
cargo build --release --locked -p cache-reuse-utils-ffi \
  --target-dir /absolute/owned/target
./ffi/tests/run-c-tests.sh /absolute/owned/target
```

The C runner defaults to `<repo>/target`, and uses the target directory argument
for both Cargo and the archive link, so a custom target cannot silently test a
stale default archive. The workspace release profile is unwind, but a downstream
Cargo consumer's root profile still controls its Git dependency build; set
`[profile.release] panic = "unwind"` there when relying on the FFI's panic
containment. Header freshness is optional and requires cbindgen 0.29.4:

```sh
CBINDGEN=/path/to/cbindgen-0.29.4 tools/check-header.sh
```
