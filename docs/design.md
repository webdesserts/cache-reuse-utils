# Design note: selector-first contract

## Dream API first

The caller experience comes first: one configured analysis takes logical context
tokens, one borrowed logical cache view, `K`, one absolute deadline and
cancellation, then returns a geometrically checked plan with timing and binding
metadata or a specific refusal. `CacheReuseAnalysis::analyze` implements Rust
discovery and one placement-aware search over actual cached tokens; native
pinning, physical execution and adapter validation remain future work. A
caller-declared `PlacementPolicy` supplies the relative loss allowance,
protected prefix/fresh suffix, insertion boundaries, optional pad token,
alignments, minimum prepared start/physical count, capacity, trim bounds and
bounded variant limits.

The result is an ordinary `Result<ReusePlan, PlanError>`: a valid plan is `Ok`
even if the deadline stopped further work; exhaustion without a plan is
`PlanError::BudgetExhausted { stage, timing }`; cancellation, admission, invalid
input and invariant failures remain distinct. `ReusePlan` and every error carry
`AnalysisTiming { elapsed, limit, reached, preparation, discovery, search }`
measured from one monotonic clock, plus a stopping stage on errors. A complete
no-match is a deliberate empty plan; incomplete discovery with no plan is an
error. The objective is an explicit relative floor followed by minimum current-call
insertions, maximum useful coverage, fewer final coalesced ranges and stable
lexicographic order. `B` is the best completed useful coverage;
`admissible_min = B - floor(B * basis_points / 10000)`. Completed buckets are
tracked by inserted-padding count, with a high-coverage fallback, so a rising
`B` cannot leave a stale below-floor incumbent.

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
actual-token cache slice for a synchronous call, discovers candidates and
returns a stateless placement recipe; it stores no pointers between calls. A
future C adapter will supply the actual-token view and opaque identity, then map
the recipe through native positions and revalidate it. No `Vec`/`String` or native
tensor handle crosses the ABI.

## Units

`source_start` and `destination_start` are logical token indices in the actual
retained source and original target sequences, and `token_count` is a token
count. A placed range additionally records its prepared start. All are `u32` and
half-open. The recipe's prepared ranges and current-call insertion counts are
returned to the caller; native physical mapping and execution stay native.

## Candidates and selected plans

Discovery emits maximal contiguous exact logical matches at least
`minimum_match_tokens` long, at every logical position. Maximal matches are
alternatives and may conflict; they remain competing candidates for the search.
This is not an enumeration of every trimmed subrange. The caller-declared
bounded trim rectangle and canonical latest-boundary/minimal-current-pad rule
define the domain; a plan is best over that declared maximal-match/trim/
placement domain, not a global claim over all substrings or native alignments. The
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
are bounded to 1,048,576 logical tokens per side; the modeled placement payload
ceiling is 32 MiB. This excludes allocator overhead and a separate O(log N) sort
stack; larger inputs are admission errors. Placement state accounting uses the
actual admitted state/index/recipe model, not a dense closed-form worst case.
No work-tick cap masquerades as a time stop. Cancellation wins over the deadline
and never authorises continuing inference with a plan, even if an incumbent
exists. Admission failure and invariant errors are separate. Timer
registration/start failure is a typed `AdmissionDeniedReason::TimerUnavailable`
at Preparation and fails closed; it is not a clock fallback. Raw candidate-pool
scratch is still modeled by `scratch_bytes`; placement variants are admitted by
the separate placement payload model, and discovery uses separate modeled
input/index admission. Placement admission includes variants, `StateKey` order
entries, state/bucket plans and insertion capacities. A 4 MiB publication reserve
covers the modeled maximum bucket payload:
`(MAX_NEW_PADDING+1) * size_of::<Option<Partial>>() +
(MAX_NEW_PADDING+1) * (size_of::<Partial>() +
K * (size_of::<PlacedRange>() + size_of::<Insertion>()))`.
This is a conservative modeled upper bound; allocator, `HashMap` table and spare
`Vec` overhead are documented exclusions, not RSS claims. Actual allocation
failures remain `AdmissionDenied`; only modeled caps become limit statuses.

## Validity is not equivalence

A range that passes arithmetic and compatibility checks is not proof that the
reused tokens match or that model state is equivalent. Token equality, source
extents and execution safety remain native obligations.

## Deliberately deferred

Historical source-pad provenance/projection, the Dwarfstar adapter and FFI,
coverage-by-K reporting, greedy fallback, parallel/interleaved search, GPU
placement, and any performance claim or benchmark. Logical maximal-match
discovery, stateless placement variants, bounded state/bucket search,
deterministic ties and best-completed-plan selection described above are
implemented in Rust.
