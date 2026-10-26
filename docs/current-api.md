# Current API (implemented)

These examples are compiled as doctests. `CacheReuseAnalysis` performs Rust
discovery over one borrowed actual-token cache view and then searches placement
variants. Native cache pinning, physical mapping and execution remain future
adapter work.

## Analyze context and a borrowed cache view

```rust
# use scr_planner::{CacheIdentity, CacheReuseAnalysis, CacheView, PlacementPolicy, PlanError, ReusePlan};
# use std::time::{Duration, Instant};
fn analyze(context: &[u32], cache: &[u32]) -> Result<ReusePlan, PlanError> {
    let mut cancel = || false;
    CacheReuseAnalysis {
        context_tokens: context,
        cache: CacheView {
            tokens: cache,
            identity: CacheIdentity::new(42),
            minimum_match_tokens: 3,
        },
        k: 2,
        deadline: Instant::now() + Duration::from_secs(5),
        cancel: &mut cancel,
        placement: PlacementPolicy::default_for(context.len()),
    }
    .analyze()
}

# fn main() -> Result<(), PlanError> {
let plan = analyze(&[1, 2, 3, 4], &[9, 1, 2, 3, 4, 9])?;
assert_eq!(plan.covered_tokens(), 4);
assert_eq!(plan.cache_identity(), CacheIdentity::new(42));
assert!(plan.timing().discovery().is_some());
assert!(plan.timing().search().is_some());
# Ok(())
# }
```

After validation and the known no-work check, analysis registers one absolute
deadline with a private background timer owner. Discovery/search read a
per-analysis notification flag at natural loop points; cancellation remains a
separate callback and is checked first. Clock reads remain at phase boundaries
for telemetry.

`ReusePlan` exposes logical `ranges()`, `covered_tokens()`, `timing()`,
`cache_identity()`, `discovery_status()`, `placement()` and `metrics()`, plus
`placement_status()` (`Complete`, `NoFeasibleReuse`, `DeadlinePartial`,
`VariantLimitReached` or `StateLimitReached`). The placement recipe contains prepared ranges, this
call's insertion records and a materialization helper. `PlacementPolicy`
defaults are no insertions, `pad_token = None`, alignment 1, no trims, no
protected prefix/fresh suffix and capacity equal to the original target length.
The default loss allowance is 1000 basis points (10%); zero is strict and 10000
is allowed, with the documented empty-plan edge. `AnalysisTiming` reports `elapsed`,
the absolute-deadline-derived `limit`, `reached`, `preparation`, optional
`discovery`, and optional `search`. Stage boundaries use one finish-checkpoint
convention; present-stage durations sum to `elapsed`.

Discovery finds maximal exact actual-token matches at every logical position, at
or above `minimum_match_tokens`. It does not apply native physical parity or
minimum-128 heuristics, and it does not enumerate every trimmed subrange. The
placement domain is the caller-declared bounded trim rectangle; canonical
latest-boundary insertions use the smallest current-call pad count satisfying
target alignment and minimum prepared start. The result is complete only over
that declared maximal-match/trim/placement domain, not a global claim. A
completed best plan is returned with an explicit partial/limit status when the
deadline or a modeled bound stops exploration; complete no-feasible placement is
`NoFeasibleReuse`. A zero minimum is invalid; a minimum longer than either
input is a complete no-match.

Complete no-match, empty input and a minimum longer than an input are deliberate
empty plans. They retain `cache_identity()` and do not fabricate discovery or
search durations. If discovery reaches the 4096 candidate capacity, exact
verified candidates are retained and `discovery_status()` reports
`CandidateLimitReached`; search uses only the remaining time. Inputs are bounded
to 1,048,576 logical tokens per side; the modeled placement payload ceiling is
32 MiB. This excludes allocator overhead and a separate O(log N) sort stack;
larger inputs return an admission error, not a silent truncation.

## Errors are ordinary, informative and distinct

```rust
# use scr_planner::{AnalysisStage, CacheIdentity, CacheReuseAnalysis, CacheView, PlacementPolicy, PlanError};
# use std::time::{Duration, Instant};
# fn main() {
let mut cancel = || true;
let result = CacheReuseAnalysis {
    context_tokens: &[],
    cache: CacheView {
        tokens: &[],
        identity: CacheIdentity::new(1),
        minimum_match_tokens: 1,
    },
    k: 1,
    deadline: Instant::now() + Duration::from_secs(5),
    cancel: &mut cancel,
    placement: PlacementPolicy::default_for(0),
}
.analyze();
assert!(matches!(
    result,
    Err(PlanError::Cancelled {
        stage: AnalysisStage::Preparation,
        ..
    })
));
# }
```

Cancellation wins at every checkpoint. Every error carries its stopping `stage`
(`Preparation`, `Discovery`, `Search` or `Finish`) and `timing`. A deadline during
discovery returns `BudgetExhausted` at `Discovery`; a deadline with verified
candidates but no time to enter search returns `BudgetExhausted` at `Search` with
`timing.search() == None`. The stage can name a phase refused entry, and absent
stage timing distinguishes that from a zero-duration phase. Admission
refusal, invalid input and invariant failures remain separate. Invalid
placement policies return `InvalidPlacementPolicy`; a variant/state limit with
no valid completed plan returns `PlacementIncomplete`. Admission refusals carry
a typed reason (`InputTooLarge { side, got, max }`, `DiscoveryBuffer
{ bytes, max }`, `Allocation`, or `TimerUnavailable`) so oversized inputs,
bounded allocations, and background-timer startup failures can be diagnosed
without changing the refusal classification.

## Identity and native boundary

`CacheIdentity` is caller-supplied opaque binding metadata, round-tripped on the
plan including empty plans. It is not staleness protection, token proof or
execution authorization. `CacheView::tokens` are actual token IDs represented by
cached positions; native owns physical positions, cache pinning, source
generation and execution. See `dream-api.md` for the future native boundary.

## Selected-plan geometry

`validate_plan` remains available for callers that already hold a plan. It checks
order, crossing/overlap, adjacency/coalescing and `K`; it does not prove token
equality, source extents or model-state equivalence.
