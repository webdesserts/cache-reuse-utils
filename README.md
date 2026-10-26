# scr-planner

Safe Rust planning for Suffix Cache Reuse (SCR). Given a target token sequence and
one borrowed cache of actual token IDs, `scr-planner` finds exact qualifying
matches, chooses a bounded placement, and returns an owned plan and recipe. The
engine adapter owns cache state, physical placement and execution.

## Quickstart

```rust
# use scr_planner::{CacheIdentity, CacheReuseAnalysis, CacheView, PlacementPolicy, PlanError, ReusePlan};
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

`analyze` validates the request, discovers maximal exact matches, and searches
under one cooperative deadline. A valid plan is returned as `Ok` even when the
deadline stops further work. Complete no-reuse is an empty plan. A deadline with
no incumbent returns `PlanError::BudgetExhausted`; a variant/state exploration cap
with no incumbent returns `PlanError::PlacementIncomplete`. Cancellation takes
precedence. The plan carries selected ranges, coverage, current-call insertion
recipe, metrics/status, identity and timing. See the rustdoc for
`PlacementPolicy`, `ReusePlan`, `PlanError` and `PlacementStatus` for the full
contracts.

`PlacementPolicy::without_padding(target_len)` performs no insertions;
`PlacementPolicy::with_padding(target_len, pad_token, allowed_boundaries,
prepared_capacity)` enables caller-owned insertion. Both start at neutral
alignment 1. `PlacementRecipe::materialize` builds the prepared target token
buffer and checks reused-token equality against the borrowed source; it does not
install native cache tensors or execute inference.

## Engine boundary

The engine adapter owns the authoritative cache snapshot, token identity,
physical positions, generation checks, allocation and execution. A returned plan
is a proposal: validate it against native state before using it. Planning does not
prove model-state equivalence or a qualified engine integration.

## Background

SCR reuses already-computed context when a new request shares an exact span with
it. Upstream Dwarfstar does not provide SCR by default; this series adds it
through integration patches.

References:

- Rulin Shao et al., *Context Language Models*, arXiv:2609.37725v1, Appendix B
  “Suffix Cache Reuse” —
  <https://arxiv.org/abs/2609.37725v1>; appendix
  <https://arxiv.org/html/2609.37725v1#A2>.
- Astra’s Dwarfstar recipe, SCR chapter —
  <https://tangled.org/astrra.space/ds4-recipe/blob/main/README.md>; chapter
  <https://next.tangled.org/astrra.space/ds4-recipe/blob/main/13-suffix-cache-reuse.md>.
  Its target runtime and numbers are inspiration, not transferable claims.

## Documentation and development

- Rustdoc is the API reference: run `cargo doc --no-deps --all-features`.
- [Design notes](docs/design.md) explain search and resource rationale.
- [Research notes](docs/futurte-research.md) collect open evaluation questions.
- [Contributing](CONTRIBUTING.md) lists build and test commands.
