# Future dream API (discovery not implemented)

`CacheReuseAnalysis::analyze` exists today as a thin wrapper over
caller-supplied candidates. The shape below is the intended later whole-analysis
call: context tokens plus a read-only cache view, with Rust discovering
candidates before search. That discovery input and matching are **not
implemented**; no cache snapshot or discovery scaffolding is exposed yet. This
example is not compiled and is not a claim about the current API.

```rust,ignore
use scr_planner::{CacheReuseAnalysis, PlanError, ReusePlan};
use std::time::{Duration, Instant};

let mut cancel = || false;
let result: Result<ReusePlan, PlanError> = CacheReuseAnalysis {
    context_tokens,                      // logical tokens of the new request
    cache: bounded_read_only_cache_view, // retained model/cache view, borrowed
    k: 6,
    deadline: Instant::now() + Duration::from_secs(5),
    cancel: &mut cancel,
}
.analyze();

match result {
    Ok(plan) => native_execute(plan), // native owns tensors/state/execution
    Err(PlanError::BudgetExhausted { timing, .. }) => fallback_without_reuse(timing),
    Err(PlanError::Cancelled { .. }) => stop_the_request(),
    Err(PlanError::AdmissionDenied { .. }) => fallback(),
    Err(PlanError::InvalidInput { reason, .. }) => surface_input_error(reason),
    Err(PlanError::Invariant { reason, .. }) => surface_internal_failure(reason),
}
```

Open design questions for later: the exact read-only cache view boundary, how
Rust discovers candidates without taking ownership of cache tensors, how source
token identity/extents are validated, and the C adapter/FFI copy boundary. Native
code remains authoritative for cache state, model-state equivalence and
execution; the Rust side returns only a plan proposal.
