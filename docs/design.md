# Design note: selector-first contract

## Dream API first

The caller experience comes first: one configured analysis takes logical context
tokens, one borrowed logical cache view, `K`, one absolute deadline and
cancellation, then returns a geometrically checked plan with timing and binding
metadata or a specific refusal. `CacheReuseAnalysis::analyze` now implements Rust
discovery before the private `CandidateSearch`; native projection, pinning,
physical placement and execution remain future adapter work.

The result is an ordinary `Result<ReusePlan, PlanError>`: a valid plan is `Ok`
even if the deadline stopped further work; exhaustion without a plan is
`PlanError::BudgetExhausted { stage, timing }`; cancellation, admission, invalid
input and invariant failures remain distinct. `ReusePlan` and every error carry
`AnalysisTiming { elapsed, limit, reached, preparation, discovery, search }`
measured from one monotonic clock, plus a stopping stage on errors. A complete
no-match is a deliberate empty plan; incomplete discovery with no plan is an
error. There is no public objective or strategy choice; the internal policy is
covered tokens, then fewer final coalesced ranges, then lexicographic normalized
final ranges.

Concrete friction to watch: a self-owned `ReusePlan` keeps the Rust call simple
but needs a deliberate copy boundary for the future C adapter and caller-owned
buffers; hiding validation/scratch/budget behind one call reduces wiring but
must still expose enough diagnostics for native admission decisions.

Background and the original references (research paper and Astra’s Dwarfstar
recipe, with their approximation caveats) are listed in the repository README
under “What SCR is”. Build and test instructions live in `CONTRIBUTING.md`.

## Ownership

Upstream Dwarfstar does not provide SCR by default; this series adds it through
our integration patches. Native code owns cache/tensor/session state, token
identity, physical extents, pinning and execution. The Rust crate borrows one
logical cache token slice for a synchronous call, discovers candidates and
returns a logical plan proposal; it stores no pointers between calls. A future C
adapter will supply the logical view and opaque identity, then map the proposal
back through native positions and revalidate it. No `Vec`/`String` or native
tensor handle crosses the ABI.

## Units

`source_start` and `destination_start` are logical token indices in the retained
source and destination sequences, and `token_count` is a token count. All are
`u32` and half-open. Placement and padded-token construction stay native.

## Candidates and selected plans

Discovery emits maximal contiguous exact logical matches at least
`minimum_match_tokens` long, at every logical position. Maximal matches are
alternatives and may conflict; they remain competing candidates for the search.
This is not an enumeration of every trimmed subrange, so the resulting plan is
best over that declared pool, not a global claim over all alignments. The
selected plan is ordered by destination then source, non-crossing,
non-overlapping, and free of touching ranges because those are coalesced before
output. `validate_plan` checks output compatibility. A complete no-match or
explicit empty input is deliberate no-reuse.

## Budget, cancellation and incumbent

`CacheReuseAnalysis` records an analysis-start checkpoint, then registers one
absolute deadline with a lazy shared timer owner after validation/no-work
handling. The owner sets a per-analysis notification flag; discovery and search
read it at natural loop points. There are no work-step counters or modulo
scheduling. Cancellation is a separate callback and is checked first; the flag is
only a notification and carries no data. Clock reads remain at phase boundaries
for `elapsed`, `limit`, `reached`, `preparation`, optional `discovery` and
optional `search`; present-stage durations sum to `elapsed`. The timer is
unregistered by RAII on early completion. A deadline before the start can set the
flag at registration. A deadline during discovery stops before new search and
yields `BudgetExhausted` at Discovery; candidates followed by expiry before
search yield `BudgetExhausted` at Search with no search duration. A bag of
candidates is not an incumbent. The deadline covers validation and Rust
preparation/discovery/search, not native prefill/decode. Final checking and
return are a small bounded tail: a cooperative soft deadline, not hard realtime.
`reached` is a finish-time telemetry observation and may differ from the timer
notification under scheduler delay.

A valid incumbent found before the deadline is returned as `Ok(plan)` even when
the deadline stopped further search. No valid incumbent found before the
deadline yields `BudgetExhausted` with the same timing; exhaustion never
fabricates an empty plan. Complete no-match/empty no-work analysis returns a
deliberate empty plan. Discovery candidate capacity is the only expected partial
limit; exact verified candidates are retained, search runs only while time
remains, and the plan reports `DiscoveryStatus::CandidateLimitReached`. Inputs
are bounded to 1,048,576 logical tokens per side; modeled heap payload is about
24.05 MiB at the cap with a 32 MiB admission ceiling. This excludes allocator
overhead and a separate O(log N) sort stack; larger inputs are admission errors.
No work-tick cap masquerades as a time stop. Cancellation wins over the deadline and
never authorises continuing inference with a plan, even if an incumbent exists.
Admission failure and invariant errors are separate. Timer registration/start
failure is a typed `AdmissionDeniedReason::TimerUnavailable` at Preparation and
fails closed; it is not a clock fallback. Search scratch is modeled by
`scratch_bytes`; discovery uses separate modeled input/index admission.

## Validity is not equivalence

A range that passes arithmetic and compatibility checks is not proof that the
reused tokens match or that model state is equivalent. Token equality, source
extents and execution safety remain native obligations.

## Deliberately deferred

Native pad projection/pinning/physical placement, the Dwarfstar adapter and FFI,
coverage-by-K reporting, greedy fallback, parallel/interleaved search, GPU
placement, and any performance claim or benchmark. Logical maximal-match
discovery, the bounded DP, deterministic ties and best-incumbent selection
described above are implemented in Rust.
