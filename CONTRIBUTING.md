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
