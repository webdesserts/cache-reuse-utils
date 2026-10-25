# Contributing

## Build and test

This crate is `std`-only with no external dependencies.

```sh
cargo fmt --check
cargo check --offline
cargo test --offline          # includes README doctests
cargo clippy --offline --all-targets -- -D warnings
```

Prefer API-level integration tests under `tests/` that exercise the public
contract. Unit tests inside a module are appropriate for complex algorithm
internals that would be awkward to drive through the public API.
