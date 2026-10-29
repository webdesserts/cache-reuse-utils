# cache-reuse-utils

Safe Rust planning for Suffix Cache Reuse (SCR). Given a target token sequence and
one borrowed cache of actual token IDs, `cache-reuse-utils` finds exact qualifying
matches, chooses a bounded placement, and returns an owned plan and recipe. The
engine adapter owns cache state, physical placement and execution.

## Quickstart

```rust
# use cache_reuse_utils::{CacheIdentity, CacheReuseAnalysis, CacheView, PlacementPolicy, PlanError, ReusePlan};
# use std::time::{Duration, Instant};
fn analyze_reuse(target: &[u32], cache: &[u32]) -> Result<ReusePlan, PlanError> {
    let mut cancel = || false; // caller-owned cancellation
    CacheReuseAnalysis {
        context_tokens: target,
        cache: CacheView {
            tokens: cache,
            identity: CacheIdentity::new(7),
            minimum_match_tokens: 4,
        },
        k: 10, // maximum final reused ranges
        deadline: Instant::now() + Duration::from_secs(5),
        cancel: &mut cancel,
        placement: PlacementPolicy::without_padding(target.len()),
    }
    .analyze()
}

# fn main() -> Result<(), PlanError> {
let plan = analyze_reuse(&[10, 20, 30, 40, 50], &[90, 10, 20, 30, 40, 50, 90])?;
assert_eq!(plan.covered_tokens(), 5);
assert_eq!(plan.cache_identity(), CacheIdentity::new(7));
# Ok(())
# }
```

`analyze` validates the request, keeps the 4,096 longest maximal exact matches,
and searches placements under one cooperative deadline. Search seeds a greedy
plan first, so a deadline still returns a good plan as `Ok` with a partial
status. Without any plan, a deadline returns `PlanErrorKind::BudgetExhausted`
and a variant or state cap returns `PlanErrorKind::PlacementIncomplete`.
Cancellation always wins. No reuse is an empty plan.

`PlacementPolicy::without_padding(target_len)` performs no insertions.
`PlacementPolicy::with_padding(target_len, pad_token, allowed, prepared_capacity)`
lets search insert pad tokens to meet prepared-start alignment, and charges 16
covered tokens per pad when choosing a plan (`pad_penalty_tokens`). Within the
loss allowance (10% by default) of the best coverage, the plan with the most
coverage net of that penalty wins. `PlacementRecipe::materialize` builds the
prepared token buffer and checks every reused token against the source; it does
not touch native cache state.

## C ABI

The separate `ffi/` package is the model-independent C ABI boundary. It builds
the `cache_reuse_utils_ffi` static library, which exports `cache_reuse_plan` and
the `cache_reuse_*` types and `CACHE_REUSE_*` constants declared in the
generated header `ffi/include/cache_reuse_utils.h`. Normal Rust and C builds do
not require cbindgen.

This is one Cargo workspace with the safe core as its default member and the FFI
crate as an explicit member. The workspace release profile uses
`panic = "unwind"` so the FFI's `catch_unwind` boundary remains effective. Build
and test the FFI explicitly:

```sh
cargo test --locked -p cache-reuse-utils-ffi
cargo build --release --locked -p cache-reuse-utils-ffi \
  --target-dir /absolute/owned/target
./ffi/tests/run-c-tests.sh /absolute/owned/target
```

`run-c-tests.sh` defaults to the workspace target at `<repo>/target`, and always
builds and links the archive under the target directory passed as its first
argument (or `CARGO_TARGET_DIR`). Compiled C test binaries are written to
`<target>/c-tests`. Link a C consumer with the include path and the archive:

```sh
cc -std=c11 -Iffi/include consumer.c \
  /absolute/owned/target/release/libcache_reuse_utils_ffi.a -lpthread -ldl -lm
```

A downstream Cargo consumer's **root** manifest controls profiles for Git
dependencies. This workspace's release unwind setting does not force a
consumer's dependency build to unwind. A consumer that relies on the C ABI
`catch_unwind` boundary, including the native static-library shim, must set
`[profile.release] panic = "unwind"` in its root profile (or otherwise verify the
effective profile).

The call contract is the existing one: input and source arrays are `int32_t`
tokens with accompanying lengths; null input pointers are accepted only for zero
lengths; config and result must be valid aligned pointers; cancellation is an
optional C callback and must not unwind; pointers are not retained after the
call. The function returns 0 for a produced plan, including an empty or partial
plan, and nonzero otherwise. Any non-null `result` is fully initialized,
including when `config` is null. Error text is truncated to the buffer and
NUL-terminated. `pad_penalty_tokens = 0` selects the default of 16 when padding
is enabled.

The optional `tools/check-header.sh` requires cbindgen 0.29.4 and checks the canonical header
against a temporary generation; it is not part of ordinary builds.

## Engine boundary

The engine adapter owns the authoritative cache snapshot, token identity,
physical positions, generation checks, allocation and execution. A returned plan
is a proposal: validate it against native state before using it. Planning does not
prove model-state equivalence or a qualified engine integration.

## Background

SCR reuses already-computed context when a new request shares an exact span with
it.

References:

- Rulin Shao et al., *Context Language Models*, arXiv:2609.37725v1, Appendix B
  “Suffix Cache Reuse” —
  <https://arxiv.org/abs/2609.37725v1>; appendix
  <https://arxiv.org/html/2609.37725v1#A2>.
- Astra’s SCR recipe —
  <https://tangled.org/astrra.space/ds4-recipe/blob/main/README.md>; chapter
  <https://next.tangled.org/astrra.space/ds4-recipe/blob/main/13-suffix-cache-reuse.md>.
  Its target runtime and numbers are inspiration, not transferable claims.

## Documentation and development

- Rustdoc is the API reference: run `cargo doc --no-deps`.
- [Design notes](docs/design.md) explain the algorithm and its bounds.
- [Behavior spec](specs/full-cache-analysis.feature) lists the contract scenarios.
- [Contributing](CONTRIBUTING.md) lists build and test commands.
