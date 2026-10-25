# scr-planner

This library is the planning side of Suffix Cache Reuse (SCR) in our Dwarfstar
integration: Dwarfstar discovers reusable spans of already-computed context, and
this crate turns those candidates into the best geometrically valid plan it can
find before an absolute deadline. `CacheReuseAnalysis` is implemented today as a
thin wrapper over caller-supplied candidates; discovery from context tokens and a
cache view is still future work.

## Use it

One configured analysis call validates candidates, searches under one absolute
deadline, and returns the best geometrically valid plan found. `cancel` is a
whole-request stop; a usable plan is returned as `Ok(plan)` even when the
deadline stops further search.

```rust
# use scr_planner::{CacheReuseAnalysis, CandidateRange, PlanError, ReusePlan};
# use std::time::{Duration, Instant};
fn analyze_reuse(candidates: &[CandidateRange]) -> Result<ReusePlan, PlanError> {
    let mut cancel = || false; // caller-owned whole-request cancellation
    CacheReuseAnalysis {
        candidates,                         // supplied by discovery/mapping today
        k: 6,                               // at most six selected ranges
        deadline: Instant::now() + Duration::from_secs(5),
        cancel: &mut cancel,
    }
    .analyze()
}

# fn main() -> Result<(), PlanError> {
let plan = analyze_reuse(&[])?; // empty input is explicit no-reuse
assert!(plan.ranges().is_empty());
assert_eq!(plan.covered_tokens(), 0);
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
  Analysis records a start checkpoint, passes the same monotonic `Instant`
  deadline through preparation and search, and records search-start and finish
  checkpoints. A deadline already in the past is handled as zero remaining time.
  Search may finish early and returns immediately (no sleeping to fill the
  budget). Final plan checking and return are a small bounded tail, so this is a
  cooperative soft deadline, not hard realtime. Native prefill/decode is not
  included.
- **Metadata lives on the plan or error.** `ReusePlan` exposes ranges, covered
  tokens and `AnalysisTiming` (`elapsed`, `limit`, `reached`, `preparation`,
  optional `search`); errors that stop an attempt carry the same timing plus the
  stage (`Preparation`, `Search` or `Finish`). `reached` only says the finish
  checkpoint met the absolute deadline; it never implies more search should occur.
- **One internal policy.** There is no public objective or strategy choice. The
  selector maximizes covered tokens; ties go to fewer final coalesced ranges,
  then the lexicographically smaller normalized final plan.
- **Empty no-reuse is deliberate.** A validated empty plan is distinct from
  having no plan at all; a deadline is never converted into an empty plan. Empty
  input returns that plan after validation without starting search, unless
  cancellation wins.
- **Refusals stay distinct.** Whole-request cancellation is checked first and is
  not a budget stop; admission failure (planner allocation/scratch), invalid
  input and invariant failures are separate. Geometry never proves token
  equality or model-state equivalence; native validation and execution remain
  authoritative.

`CacheReuseAnalysis` currently takes caller-supplied candidates. Its planned
input change to context tokens plus a bounded read-only cache view, with
discovery running before search, is described in
[`docs/dream-api.md`](docs/dream-api.md) and is **not implemented**. This public
wrapper is not completed discovery or cache analysis.

## What SCR is

Suffix Cache Reuse lets Dwarfstar reuse context that has already been computed
when a new request shares a suffix or span with it, avoiding a full recomputation.
This crate targets the planning side of that integration; matching, token
identity, source bounds, model-state equivalence and execution safety stay
native. Upstream Dwarfstar does not provide SCR by default — this series adds it
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

- [x] **Working thin `CacheReuseAnalysis` over caller-supplied candidates.**
  Validation, one absolute deadline shared by preparation and search, best valid
  incumbent on deadline, cancellation precedence, coalescing/K geometry, owned
  `ReusePlan` timing metadata; covered by public API, exhaustive-oracle and
  fake-clock tests.
- [ ] **Full context/cache analysis.** `CacheReuseAnalysis` gains context-token
  and read-only cache-view inputs; Rust discovers candidates before search, then
  returns a reuse plan. Discovery is not implemented today and the public wrapper
  must not be read as completing it. Future design:
  [`docs/dream-api.md`](docs/dream-api.md).
- [ ] **Dwarfstar adapter and native validation.** Source token identity/extents,
  cache-state compatibility, allocation admission and a conservative FFI
  boundary. Native code still owns cache state and execution.
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
- [Future dream API](docs/dream-api.md): planned context/cache discovery input,
  not implemented today.
- [Future research](docs/futurte-research.md): search-quality and performance
  questions, evaluation ideas and testing notes.
- [Contributing](CONTRIBUTING.md): build and test instructions.
