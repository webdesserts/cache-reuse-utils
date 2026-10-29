# cache-reuse-utils

Safe, stateless Rust planning for Suffix Cache Reuse (SCR). Given a target
prompt and one borrowed KV-cache token slice, `cache-reuse-utils` finds exact
shared token spans, searches for the highest-value placement that satisfies
hardware alignment and range limits, and returns an owned reuse plan and
recipe. The host engine adapter remains responsible for cache lifetime,
physical block layout, and execution.

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

### How `analyze()` behaves

`analyze()` validates the request, discovers up to 4,096 of the longest maximal
exact matches, and searches for the best valid placement within a single
cooperative deadline:

- **Empty plans vs. errors:** When no match meets `minimum_match_tokens` (or
  when matches are found but none can be placed legally under the policy),
  `analyze()` succeeds with an empty `ReusePlan`.
- **Graceful deadline fallback:** Before running the full dynamic-programming
  search, the planner seeds a fast greedy incumbent from the longest matches.
  If the deadline or a state cap stops the search early, `analyze()` still
  returns `Ok` with the best plan found so far and a partial
  `PlacementStatus`. It returns `Err(PlanErrorKind::BudgetExhausted)` or
  `Err(PlanErrorKind::PlacementIncomplete)` only if a limit was hit before any
  valid plan could be constructed.
- **Cancellation priority:** The caller's `cancel` callback is checked ahead of
  the clock at every check point, so cancellation always returns
  `Err(PlanErrorKind::Cancelled)` and discards any partial incumbent.

### Alignment, padding, and left-trimming

Hardware engines often require reused cache spans to start on aligned token
boundaries (for example, multiples of 8 or 16 tokens in the prepared prompt).
When a candidate match does not land on an aligned target boundary, there are
two ways to align it:

1. **Trim the reuse boundary (zero padding):** Leave the first few tokens of
   the match to be computed fresh as normal prefill so that the reused slice
   starts cleanly on the next aligned boundary. This does not remove tokens
   from the prompt or alter model semantics; it simply reuses slightly fewer
   cached tokens.
2. **Insert padding tokens:** When configured with
   `PlacementPolicy::with_padding(target_len, pad_token, allowed, prepared_capacity)`,
   the planner can also insert `pad_token` at the latest allowed boundary
   before the match to shift the match start onto an aligned index.

Because inserting pad tokens mutates the prompt, each inserted pad token carries
a coverage penalty during selection (`pad_penalty_tokens`, which defaults to 16
tokens). Among all plans whose total coverage falls within the configured
`LossAllowance` (10% by default) of the maximum raw coverage found, the planner
chooses the plan with the highest **net coverage**
(`covered_tokens - inserted_padding * pad_penalty_tokens`).

`PlacementRecipe::materialize` constructs the prepared token buffer and verifies
that every reused span matches the original target tokens; it never touches
native engine state.

## C ABI

The `ffi/` crate provides a model-independent C ABI boundary. It builds the
`cache_reuse_utils_ffi` static library, which exports `cache_reuse_plan` along
with the `cache_reuse_*` structs and `CACHE_REUSE_*` constants declared in the
checked-in header `ffi/include/cache_reuse_utils.h`. Ordinary Rust and C builds
do not require `cbindgen`.

The repository is a single Cargo workspace: the safe core crate is the default
member and `ffi/` is an explicit workspace member. The workspace release
profile sets `panic = "unwind"` so the FFI boundary can catch panics via
`catch_unwind` before they cross into C.

Build and test the FFI crate explicitly:

```sh
cargo test --locked -p cache-reuse-utils-ffi
cargo build --release --locked -p cache-reuse-utils-ffi \
  --target-dir /absolute/owned/target
./ffi/tests/run-c-tests.sh /absolute/owned/target
```

`run-c-tests.sh` defaults to `<repo>/target`, builds and links the static
archive under the target directory passed as its first argument (or
`CARGO_TARGET_DIR`), and writes compiled C test binaries to `<target>/c-tests`.
To link a C consumer against the header and static library:

```sh
cc -std=c11 -Iffi/include consumer.c \
  /absolute/owned/target/release/libcache_reuse_utils_ffi.a -lpthread -ldl -lm
```

**Note on Cargo release profiles:** In Cargo, the **root** workspace manifest
controls profile settings for all dependencies, including Git dependencies. If
a downstream Rust crate links `cache-reuse-utils-ffi` and relies on its
`catch_unwind` boundary, that downstream workspace must set
`[profile.release] panic = "unwind"` in its own root `Cargo.toml`.

### C call contract

- `target` and `source` pass `int32_t` token arrays with explicit lengths; null
  token pointers are accepted only when the corresponding length is zero.
- `config` and `result` must be valid, aligned pointers. Any non-null `result`
  is zero-initialized and populated with a status code before `cache_reuse_plan`
  returns, even if `config` is null.
- `cancel` is an optional C callback that must not unwind across the FFI
  boundary.
- No pointers are retained after `cache_reuse_plan` returns.
- A return value of `0` indicates that a plan was produced (including an empty
  or partial plan); a nonzero return value indicates an error, with the status
  in `result->status` and a NUL-terminated message in `error`.
- Setting `pad_penalty_tokens = 0` in `CacheReuseConfig` selects the default
  penalty (`16`) whenever padding is enabled.

To verify that `ffi/include/cache_reuse_utils.h` matches the Rust definitions
(requires `cbindgen` 0.29.4; writes to a temporary file without touching the
tracked header):

```sh
CBINDGEN=/path/to/cbindgen-0.29.4 ./tools/check-header.sh
```

## Engine boundary

The host engine adapter owns the authoritative cache snapshot, token identity,
physical block coordinates, generation checks, allocation, and execution. A
`ReusePlan` is a structural proposal: the adapter must validate it against
current native cache state before executing it.

## Background

Suffix Cache Reuse (SCR) accelerates prefill by reusing previously computed KV
cache blocks whenever a new prompt shares exact token spans with cached
context, even when those spans do not share a common prefix.

References:

- Rulin Shao et al., *Context Language Models*, arXiv:2609.37725v1, Appendix B
  (“Suffix Cache Reuse”) —
  <https://arxiv.org/abs/2609.37725v1>
  ([HTML appendix](https://arxiv.org/html/2609.37725v1#A2)).
- Astra’s SCR recipe —
  <https://tangled.org/astrra.space/ds4-recipe/blob/main/README.md>
  ([Chapter 13: Suffix Cache Reuse](https://next.tangled.org/astrra.space/ds4-recipe/blob/main/13-suffix-cache-reuse.md)).

## Documentation and development

- API reference: `cargo doc --no-deps`
- Algorithm and complexity guide: [docs/design.md](docs/design.md)
- Behavioral specification: [specs/full-cache-analysis.feature](specs/full-cache-analysis.feature)
- Build and test commands: [CONTRIBUTING.md](CONTRIBUTING.md)
