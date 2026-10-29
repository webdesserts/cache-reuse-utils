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
        k: 10, // maximum final reused ranges, up to MAX_RANGES (32)
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
tokens). Trimming never changes the prompt, so with the default the planner
pads only when one pad run realigns many later ranges at once, or when a
match cannot be trimmed.

### How many ranges to use

A plan may use up to `k` ranges, and `k` can be as high as `MAX_RANGES` (32),
enough for a prompt made of many unchanged blocks. Each range is a separate
reuse step for the engine, though, so a range should earn its place. Every
range after the first costs `range_penalty_tokens` (32 by default) during
selection. A plan that splits into nine more ranges to cover 15 more tokens
loses to the simpler plan, while a range that brings back a whole block of
hundreds of tokens is always worth it. The first range is free, so a single
short match is never rejected just for being one range.

### Choosing a plan

Among all plans whose total coverage falls within the configured
`LossAllowance` (10% by default) of the maximum raw coverage found, the planner
chooses the plan with the highest **net coverage**:

```text
covered_tokens
  - inserted_padding * pad_penalty_tokens
  - (ranges - 1) * range_penalty_tokens
```

The penalties only choose between plans inside the allowance. With a zero
allowance, the plan with the most coverage is required whatever its cost.

`PlacementRecipe::materialize` builds the prepared token sequence and verifies
that every reused span matches the original target tokens without touching
native engine state. `PlacementRecipe::materialize_into` writes the prepared
tokens (and, optionally, the physical index of every inserted pad) directly
into caller-supplied slices without allocating. Each `Insertion` carries both
its original boundary (`before_target_index`) and its physical start index
(`prepared_start`), matching `PlacedRange`.

`ReusePlan::discovered_match_count` and `ReusePlan::longest_match_tokens`
report raw discovery overlap before placement so callers can record overlap
telemetry without a separate pass.

### Physical and logical coordinates

When padding is inserted, an engine's KV cache holds a **physical** sequence
(the logical prompt tokens interleaved with pad tokens). Given a strictly
increasing slice of physical `pad_positions` (as written by
`materialize_into`), the stateless helpers `validate_pad_positions`,
`project_to_logical`, `logical_boundary`, and `common_logical_prefix` convert
between physical and logical coordinates. Pads are identified strictly by
position—never by token value—so ordinary prompt tokens that happen to share the
pad token ID are preserved intact.

`CoordinateMap` owns a validated physical length and pad set. The matching
free functions maintain one as a sequence changes: `verify_matches_logical`
checks a physical sequence against its logical one in `O(N)` without
allocating, `slice_pad_positions`, `append_unpadded` and
`truncate_pad_positions` follow a slice, a generated append and a truncation,
and `verify_span_correspondence` checks that a reused range carries its source
pads to the same offsets in the destination.

### Planning helpers for engines

Stateless helpers cover the decisions around executing a plan. Each takes
only coordinates and caller policy, so it runs and tests without an engine.

- **Separators** (`collect_separator_boundaries`, `verify_separator_map`,
  `SeparatorPolicy`): where a pad may be inserted (next to a caller-chosen
  separator token), and whether a padded sequence uses only authorized pads
  beyond a carried prefix.
- **Candidate selection** (`score_candidate`, `select_candidate`): keep the
  hot cache, restore a pool entry, or start fresh, from each candidate's
  aligned common prefix and caller-planned reuse, with a restore penalty.
- **Reconciliation** (`clip_ranges_to_archive`, `reconcile_executed_ranges`):
  fit ranges to an archive's source window and an aligned destination cap,
  and check that executed ranges are an ordered, aligned subset of the plan,
  accounting for reused, gap, suffix and fresh tokens.
- **Archive schedule** (`compile_archive_schedule`, `verify_archive_schedule`):
  compile ranges and an `ArchiveGeometry` (arena size, import chunk, encoder
  residency thresholds, head alignment) into an ordered list of `ArchiveOp`s
  that imports or prefills every token once, in order. The schedule never
  overfills the arena between flushes and releases encoder residency before
  each prefill segment's tail rows and before any one-token step.

## C ABI

The `ffi/` crate provides a model-independent C ABI boundary. It builds the
`cache_reuse_utils_ffi` static library, which exports `cache_reuse_plan`,
`cache_reuse_materialize`, the coordinate helpers
(`cache_reuse_project_logical`, `cache_reuse_logical_boundary`,
`cache_reuse_common_prefix`, `cache_reuse_verify_matches_logical`,
`cache_reuse_slice_pad_positions`, `cache_reuse_append_unpadded`,
`cache_reuse_truncate_pad_positions`, `cache_reuse_verify_span_correspondence`)
and the planning helpers (`cache_reuse_collect_separator_boundaries`,
`cache_reuse_verify_separator_map`, `cache_reuse_select_candidate`,
`cache_reuse_clip_ranges_to_archive`, `cache_reuse_reconcile_executed_ranges`,
`cache_reuse_compile_archive_schedule`) along with the `cache_reuse_*` structs and `CACHE_REUSE_*` constants declared in the
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
- Setting `range_penalty_tokens = 0` selects the default range penalty (`32`).
  Use `1` for the smallest penalty.
- `max_ranges` accepts `1..=CACHE_REUSE_MAX_RANGES` (32). The result holds up
  to 32 ranges and 32 insertions, about 1.1 KiB, so it still fits on the stack.
- `cache_reuse_materialize` rebuilds the recipe from a successful `result` and
  writes the prepared tokens (and, optionally, pad positions) into caller
  buffers of exactly `result->prepared_tokens` (and `result->inserted_padding`)
  elements. It and the coordinate helpers return `CACHE_REUSE_COMPLETE`,
  `CACHE_REUSE_ERR_INVALID` or `CACHE_REUSE_ERR_PANIC`, and borrow token
  buffers without copying; buffers passed to one call must not overlap.
- The planning helpers return the same three statuses. A failed check is a
  result, not an error: `cache_reuse_verify_*` report it through a `bool`
  output. Outputs are zeroed before validation. When its op buffer is too
  small, `cache_reuse_compile_archive_schedule` returns
  `CACHE_REUSE_ERR_INVALID` with `*out_op_len` set to the capacity needed.

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
