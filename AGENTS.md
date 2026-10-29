# cache-reuse-utils

- Review scope: keep commits small and explain the contract they change.
- Prefer API-level integration tests under `tests/` (how a caller uses the
  public contract). Unit tests inside a module are fine for genuinely complex
  algorithm internals that would be awkward to drive through the public API.
- Human build/test instructions are in `CONTRIBUTING.md`.
- After tests/fmt/clippy/doc and saving the evidence you need, clean only this
  repository's build artifacts:

  ```sh
  cargo clean --target-dir <repo>/target
  ```

  Keep the target directory at `<repo>/target`; never clean a shared cache or
  another workspace.
- Keep the core crate `std`-only with no dependencies, no `unsafe`, no
  threads and no retained C pointers or session ownership; native state stays
  in the engine adapter. The separate `ffi` crate is the only authorized
  unsafe/C ABI boundary.
- Comments and commits are reviewed by Michael before further work.
