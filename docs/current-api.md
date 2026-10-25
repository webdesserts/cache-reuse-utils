# Current API (implemented)

These examples are compiled as doctests. `CacheReuseAnalysis` is implemented as
a thin wrapper over caller-supplied candidates. Discovery from context tokens
and a cache view is still proposed; see `dream-api.md`.

## Analyze discovery-supplied candidates

```rust
# use scr_planner::{CacheReuseAnalysis, CandidateRange, PlanError, ReusePlan};
# use std::time::{Duration, Instant};
fn analyze(candidates: &[CandidateRange]) -> Result<ReusePlan, PlanError> {
    let mut cancel = || false;
    CacheReuseAnalysis {
        candidates,
        k: 2,
        deadline: Instant::now() + Duration::from_secs(5),
        cancel: &mut cancel,
    }
    .analyze()
}

# fn main() -> Result<(), PlanError> {
let plan = analyze(&[])?; // no candidates means explicit no-reuse
assert_eq!(plan.covered_tokens(), 0);
assert!(!plan.timing().reached());
assert!(plan.timing().search().is_none());
# Ok(())
# }
```

`ReusePlan` exposes `ranges()`, `covered_tokens()` and `timing()`.
`AnalysisTiming` reports `elapsed`, the absolute-deadline-derived `limit`,
`reached`, `preparation` and optional `search` durations. Empty input is
deliberately no-reuse: validation succeeds, no search starts, and `search()` is
`None`.

## Errors are ordinary, informative and distinct

```rust
# use scr_planner::{AnalysisStage, CacheReuseAnalysis, CandidateRange, PlanError};
# use std::time::{Duration, Instant};
# fn main() {
let mut cancel = || true;
let result = CacheReuseAnalysis {
    candidates: &[] as &[CandidateRange],
    k: 1,
    deadline: Instant::now() + Duration::from_secs(5),
    cancel: &mut cancel,
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

Cancellation wins even for empty input. Each error carries its stopping `stage`
(`Preparation`, `Search` or `Finish`) and `timing`. With no valid plan found
before the deadline the error is `BudgetExhausted { stage, timing }`; with a
valid incumbent the deadline yields `Ok(plan)`. Admission refusal (planner
allocation/scratch), invalid input and invariant failures are separate.

## Selected-plan geometry

`validate_plan` remains available for callers that already hold a plan. It checks
order, crossing/overlap, adjacency/coalescing and `K`; it does not prove token
equality, source extents or model-state equivalence.
