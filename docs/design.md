# Design notes

This note explains the planner's search and resource model.

## Discovery domain

The planner borrows one actual-token cache source and an original target. It
discovers maximal exact contiguous matches at every target boundary, at least
`minimum_match_tokens` long. These maximal matches are alternatives and may
overlap; they are not an enumeration of every trimmed substring. A plan is
complete only over the declared maximal-match, trim-rectangle and placement
domain, not over every possible substring or native alignment.

Historical pad or whitespace tokens are ordinary token values. Only insertion
records created by the current call are padding. Source coordinates, cache
generations and physical placement remain adapter responsibilities.

## Placement and search

Each maximal match can be trimmed within the policy's bounded left/right
rectangle. The search places a valid range into a fresh gap at the canonical
latest permitted boundary, using the smallest current-call padding that satisfies
the declared alignments and minimum prepared start. Adjacent compatible ranges
are coalesced and consume one final K slot; non-adjacent ranges consume separate
slots. Final ranges are ordered, non-crossing and non-overlapping.

The search proceeds in stages:

1. derive the bounded trim variants;
2. place variants and extend valid plans, respecting prefix/suffix, capacity and
   insertion permissions;
3. publish completed best-by-padding plans into buckets while retaining
   extension frontiers in separate states;
4. compute the best useful coverage `B` and the relative admissible floor;
5. choose the least-padding eligible completed plan.

The achievable floor is `B - floor(B * basis_points / 10000)`. Within that floor,
the planner minimizes current-call padding, then maximizes useful tokens, then
prefers fewer final ranges and stable lexicographic order. A high-coverage
fallback prevents an early below-floor incumbent from hiding a later better
plan. Buckets store completed plans; extension states may continue to grow.
Publishing a completed bucket before admitting its extension state keeps the
valid incumbent available if later modeled state admission reaches its limit.

## Deadline, cancellation and limits

Analysis uses one absolute monotonic deadline and a caller cancellation callback.
Cancellation wins at each checkpoint. The deadline is cooperative; it is checked
at natural discovery/search iterations and phase boundaries. A valid incumbent is
returned with a partial status when a deadline or exploration cap stops further
search. Without an incumbent, deadline exhaustion is a typed error rather than an
empty plan. Exact stage timing is recorded for telemetry.

A bag of candidates is not an incumbent. Discovery may stop at its candidate
capacity while retaining verified matches. Variant and modeled-state caps are
exploration/resource limits: with a valid incumbent they produce a partial status,
and without one they produce an incomplete refusal.

A configured `max_new_padding` above `MAX_NEW_PADDING` is invalid policy. A
candidate requiring more padding than the caller's declared `max_new_padding` is
outside the feasible domain; that may yield a smaller valid plan or a complete
no-reuse result, but it is not a policy error and does not by itself produce an
incomplete refusal.

Placement admission uses the actual modeled variant/state/recipe payload, not a
dense closed-form worst case. The publication reserve covers the maximum modeled
bucket payload; bucket storage is retained so a later state limit cannot discard a
completed plan already found. Allocator overhead and hash-table/spare-capacity
details are outside that model. The three numeric caps are provisional engineering
bounds; they are not calibrated workload percentiles and real-history measurement
is still pending.

## Validity boundary

Structural and token-content checks are not equivalence proofs. A valid plan does
not establish that native cache state matches the source, that physical positions
are executable, or that model-state reuse is safe. The adapter must revalidate
tokens, bounds, alignments, generations and cache eligibility before execution.

## Deferred work

Native projection/pinning and execution are separate adapter work. Parallel or
interleaved search, alternative fallback policies, coverage-by-K reporting and
performance calibration are not part of this planner revision.
