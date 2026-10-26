# scr-planner

This library is the planning side of Suffix Cache Reuse (SCR) in our Dwarfstar
integration: Dwarfstar owns cache state and execution, while this crate discovers
qualifying exact matches in a borrowed actual-token cache view and turns them into the
best geometrically valid plan it can find before an absolute deadline.

## Use it

One configured analysis call validates the inputs, discovers maximal
exact matches, searches under one absolute deadline, and returns an owned plan.
`cancel` is a whole-request stop; a usable plan is returned as `Ok(plan)` even
when the deadline stops further work.

```rust
# use scr_planner::{CacheIdentity, CacheReuseAnalysis, CacheView, PlacementPolicy, PlanError, ReusePlan};
# use std::time::{Duration, Instant};
fn analyze_reuse(context: &[u32], cache: &[u32]) -> Result<ReusePlan, PlanError> {
    let mut cancel = || false; // caller-owned whole-request cancellation
    CacheReuseAnalysis {
        context_tokens: context,
        cache: CacheView {
            tokens: cache,
            identity: CacheIdentity::new(7),
            minimum_match_tokens: 4,
        },
        k: 6, // at most six selected ranges
        deadline: Instant::now() + Duration::from_secs(5),
        cancel: &mut cancel,
        placement: PlacementPolicy::default_for(context.len()),
    }
    .analyze()
}

# fn main() -> Result<(), PlanError> {
let plan = analyze_reuse(&[10, 20, 30, 40, 50], &[90, 10, 20, 30, 40, 50, 90])?;
assert_eq!(plan.covered_tokens(), 5);
assert_eq!(plan.cache_identity(), CacheIdentity::new(7));
assert!(plan.timing().discovery().is_some());
assert!(plan.timing().search().is_some());
# Ok(())
# }
```

Why this shape:

- **A usable plan is never an error.** If the search has found a valid plan,
  `analyze()` returns `Ok(plan)`, including when the deadline stops further
  search. With no valid plan found before the deadline it returns
  `PlanError::BudgetExhausted { stage, timing }`; callers handle that normally
  rather than panicking or inventing an empty plan.
- **One absolute deadline, not opaque ticks.** The five seconds are illustrative.
  After validation/no-work handling, analysis registers one monotonic `Instant`
  deadline with a background timer owner; discovery/search read a per-analysis
  notification flag at natural loop points, with cancellation checked first.
  Clock reads record stage-start and finish checkpoints for telemetry. A deadline already in the past is handled as zero
  remaining time. Work may finish early and returns immediately (no sleeping to
  fill the budget). Final checking and return are a small bounded tail, so this
  is a cooperative soft deadline, not hard realtime. Native prefill/decode is
  not included.
- **Metadata lives on the plan or error.** `ReusePlan` exposes logical ranges,
  useful coverage, a placement recipe (prepared ranges and current-call
  insertions), placement metrics/status, cache identity, discovery status and
  `AnalysisTiming` (`elapsed`, `limit`, `reached`, `preparation`, optional
  `discovery`, optional `search`); only this call's insertions count as padding.
  Source and target tokens are actual content; historical padding or whitespace
  is never classified by token value. `PlacementRecipe::materialize` is a
  convenience helper, not execution authority;
  errors carry timing plus the stage (`Preparation`, `Discovery`, `Search` or
  `Finish`). `reached` only says the finish checkpoint met the deadline; it never
  implies more work should occur.
- **One placement-aware policy.** `PlacementPolicy` declares loss allowance,
  protected prefix/fresh suffix, allowed insertion boundaries, an optional
  caller pad token, positive alignments, minimum prepared start/physical count,
  capacity, trim bounds and bounded variant/state limits. Defaults use no
  insertions, no pad token, alignment 1, no trims and the target length as
  capacity, reproducing the prior no-pad pool. The default 10% relative
  allowance (`B - floor(B * 1000 / 10000)`) keeps higher-coverage plans from
  being replaced by a much smaller lower-padding plan; within the allowance it
  minimizes current-call insertions, then maximizes useful tokens, then fewer
  final ranges and stable order. `B` is the best completed plan, and a
  high-coverage fallback remains available.
- **Empty no-reuse is deliberate.** A complete no-match, empty input or a
  minimum longer than an input is a valid empty plan. Incomplete discovery with
  no verified plan is an error, never a fabricated empty plan. No-work no-match
  remains complete even if the deadline is already past.
- **Discovery has bounded, explicit quality.** Discovery enumerates maximal
  exact actual-token matches at every logical position, at least
  `minimum_match_tokens` long. The placement domain adds the caller's bounded
  trim rectangle and canonical latest-boundary/minimal-current-pad rule, so a
  plan is best over the declared maximal-match/trim/placement domain, not a
  global claim over all substrings or native alignments. The 32 MiB modeled
  placement payload and 4096-variant/default state limits are explicit; a
  completed best plan is returned with `DeadlinePartial`, `VariantLimitReached`
  or `StateLimitReached` when unresolved, never silently replaced by an empty
  plan. A complete search with no non-empty feasible placement returns an empty
  plan with `NoFeasibleReuse`; at 100% allowance an empty plan may compete only
  after a positive completed plan establishes `B`. If the 4096-candidate capacity is reached, verified candidates are
  retained, search still uses only remaining time, and the plan reports
  `DiscoveryStatus::CandidateLimitReached`. Inputs are bounded to 1,048,576
  logical tokens per side; the modeled placement payload ceiling is 32 MiB,
  excluding allocator overhead and a separate O(log N) sort stack. Larger inputs
  are admission errors rather than silent truncation.
- **Refusals stay distinct.** Whole-request cancellation is checked first and is
  not a budget stop; admission refusal carries a typed input/buffer/allocation/
  timer reason, while invalid input and invariant failures remain separate. Geometry and identity metadata never prove token equality,
  model-state equivalence or native placement; native validation and execution
  remain authoritative.

## What SCR is

Suffix Cache Reuse lets Dwarfstar reuse context that has already been computed
when a new request shares a suffix or span with it, avoiding a full recomputation.
This crate targets the planning side of that integration: it discovers exact
actual-token matches and returns a stateless plan plus prepared placement
recipe. Native token identity/extents, physical placement, cache-state
compatibility, model-state equivalence and execution safety stay native. Upstream Dwarfstar does not provide SCR by default — this series adds it
through our integration patches. Approximate reuse can depend on the old
left-context being close enough to the new one.

References:

- Rulin Shao et al., *Context Language Models*, arXiv:2609.37725v1 (29 September
  2026), Appendix B “Suffix Cache Reuse” —
  <https://arxiv.org/abs/2609.37725v1>; appendix
  <https://arxiv.org/html/2609.37725v1#A2>.
- Astra’s Dwarfstar recipe on Tangled —
  <https://tangled.org/astrra.space/ds4-recipe/blob/main/README.md>, SCR chapter
  <https://next.tangled.org/astrra.space/ds4-recipe/blob/main/13-suffix-cache-reuse.md>.
  It targets an AMD Strix Halo V4.1 runtime and describes ideas, not a blueprint;
  its numbers and settings are not claimed to transfer here.

## Current status and TODO

- [x] **Working full Rust `CacheReuseAnalysis` over a borrowed actual-token
  cache view.** Context/cache inputs, explicit minimum qualifying length, exact
  maximal match discovery, candidate-cap status, one absolute deadline across
  preparation/discovery/search, stateless placement policy/recipe, relative
  loss allowance, current-call padding and prepared mapping, cancellation
  precedence, coalescing/K geometry, identity and timing metadata; covered by
  public API, independent small-domain oracle and controlled-clock tests.
- [ ] **Dwarfstar adapter and native validation.** Native projection/pinning,
  physical positions, cache-state compatibility, allocation admission and a
  conservative FFI boundary. Native code still owns cache state and execution.
- [ ] **Measured performance and bounded parallel search.** Benchmarks/cost model
  and parallel candidate evaluation only after the correctness baseline; no
  speed claim yet.

A checked item means implemented and verified for its stated scope in the commit
that completed it; it is not human approval or deployment. Larger items are
future/proposed, not settled promises.

## Documentation

- [Current API](docs/current-api.md): executable examples and error behavior.
- [Design notes](docs/design.md): ownership, units, candidates, deadlines and
  validity boundaries.
- [Future dream API](docs/dream-api.md): native adapter and placement boundary,
  not implemented today.
- [Future research](docs/futurte-research.md): search-quality and performance
  questions, evaluation ideas and testing notes.
- [Contributing](CONTRIBUTING.md): build and test instructions.
