# Design note: selector-first contract

## Dream API first

We designed the caller experience before the implementation: one configured
analysis takes caller-supplied candidates, `K`, one absolute monotonic deadline
and a side-by-side cancellation handle, and returns either a geometrically
checked plan with timing metadata or a specific refusal. That thin wrapper is now
implemented as `CacheReuseAnalysis::analyze`; the private `CandidateSearch` is
only the existing candidate search behind it. The larger dream input (context
tokens + bounded read-only cache view, Rust discovering candidates internally)
is recorded in `docs/dream-api.md` and remains unimplemented. The public wrapper
does not complete discovery or cache analysis.

The result is an ordinary `Result<ReusePlan, PlanError>`: a valid plan is `Ok`
even if the deadline stopped further search; exhaustion without a plan is
`PlanError::BudgetExhausted { stage, timing }`; cancellation, admission, invalid
input and invariant failures remain distinct. `ReusePlan` and every error carry
`AnalysisTiming { elapsed, limit, reached, preparation, search }` measured from
one monotonic clock, plus a stopping stage on errors. A validated empty plan is
distinct from no plan. There is no public objective or strategy choice; the
internal policy is covered tokens, then fewer final coalesced ranges, then
lexicographic normalized final ranges.

Concrete friction to watch: a self-owned `ReusePlan` keeps the Rust call simple
but needs a deliberate copy boundary for the future C adapter and caller-owned
buffers; hiding validation/scratch/budget behind one call reduces wiring but
must still expose enough diagnostics for native admission decisions.

Background and the original references (research paper and Astra’s Dwarfstar
recipe, with their approximation caveats) are listed in the repository README
under “What SCR is”. Build and test instructions live in `CONTRIBUTING.md`.

## Ownership

Upstream Dwarfstar does not provide SCR by default; this series adds it through
our integration patches. The caller (native code) owns candidate discovery,
token identity, bounds, scratch admission and all session/archive state. The
Rust crate borrows only the candidate slice for one synchronous call and returns
a plan proposal; it stores no pointers between calls and owns no session. A
future C adapter will use fixed-width POD inputs and an opaque budget/cancel
context; no `Vec`/`String` crosses the ABI.

## Units

`source_start` and `destination_start` are logical token indices in the retained
source and destination sequences, and `token_count` is a token count. All are
`u32` and half-open. Placement and padded-token construction stay native.

## Candidates and selected plans

Candidates are alternatives and may conflict; the input pool is order-agnostic
and has no crossing/overlap rule. The selected plan is different: it must be
ordered by destination then source, non-crossing, non-overlapping, and free of
touching ranges because those are coalesced before output. `validate_pool`
checks input arithmetic; `validate_plan` checks output compatibility. An empty
pool/plan is explicit no-reuse.

## Budget, cancellation and incumbent

`CacheReuseAnalysis` records an analysis-start checkpoint, then passes the same
absolute `Instant` deadline unchanged through preparation and search. The
private clock computes remaining time internally with saturating arithmetic, so a
deadline before the start safely means zero remaining time. Checkpoints record
search-start and finish/error; the wrapper derives `elapsed`, `limit`,
`reached`, `preparation` and optional `search`. Cancellation is checked first at
each cooperative point, then whether `now >= deadline`. A future discovery stage
would add its own checkpoint; absent discovery is not fabricated as work. The
deadline covers validation, admission and search up to a finished result; it does
not cover native prefill/decode. Search may finish early and return immediately;
the planner never sleeps to fill the deadline. Final plan validation and
returning the result form a small bounded tail that may exceed the deadline
slightly: this is a soft deadline, not hard realtime.

A valid incumbent found before the deadline is returned as `Ok(plan)` even when
the deadline stopped further search. No valid incumbent found before the
deadline yields `BudgetExhausted` with the same timing; exhaustion never
fabricates an empty plan.
An empty plan returned deliberately is the explicit no-reuse result. No work-tick
cap masquerades as a time stop; any future internal safety bound must be an
explicit refusal. Cancellation wins over the deadline and never authorises
continuing inference with a plan, even if an incumbent exists. Admission failure
and invariant errors are separate refusals. Scratch is modeled by `scratch_bytes`
and admitted by native before allocation; the model excludes spare vector
capacity and allocator overhead.

## Validity is not equivalence

A range that passes arithmetic and compatibility checks is not proof that the
reused tokens match or that model state is equivalent. Token equality, source
extents and execution safety remain native obligations.

## Deliberately deferred

Coverage-by-K reporting, greedy fallback, discovery from a live cache view,
parallel search, the Dwarfstar/native adapter and FFI, GPU placement, and any
performance claim or benchmark. The bounded DP, deterministic ties and
best-incumbent selection described above are implemented in the selector.
