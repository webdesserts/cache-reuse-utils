# Future research: search quality and performance

This note records research directions for later work. Nothing here is
implemented, scheduled or a performance claim. The current contract and error
behavior remain authoritative in [`design.md`](design.md) and
[`dream-api.md`](dream-api.md).

## Current baseline

Each candidate is an anchor with equal-length source and destination intervals
and a token weight. A selected chain must be ordered and non-overlapping on both
axes. The planner maximizes covered tokens, then prefers fewer final coalesced
ranges, then the lexicographically smaller normalized final plan. `K` bounds
final coalesced groups: exact adjacency on both axes continues a group at zero
extra cost, while a gap starts a new group and costs one.

The current std-only search is a bounded dynamic program over this implicit
acyclic compatibility DAG. It is weighted monotone/collinear anchor chaining, not
a generic one-dimensional interval scheduler and not a general independent-set
problem. The current baseline is deliberately small and transparent; no profiling
here establishes that search, rather than discovery or native prefill/decode,
dominates an integrated request.

No suitable verified drop-in Rust library was identified for this exact
combination of two-axis intervals, coalesced-group budget, deterministic ties and
deadline incumbent. That is not a claim that none exists.

## Priority: plan quality under the deadline

The product priority is the quality of the best valid reuse plan under the
requested deadline, not minimizing search runtime. The illustrative five seconds
in examples is not a hard-coded constant; callers supply one absolute deadline.
If a complete search finishes early, it should return immediately rather than
burn the remaining budget.

If representative testing shows the current search consistently reaching the
illustrative deadline, investigate alternatives by comparing the best valid plan
each produces within the **same** budget, on the **same** candidate pools and
constraints. A faster algorithm that returns a worse plan at the deadline is not
an improvement. Judge plans with the existing hierarchy—coverage, fewer final
coalesced ranges, then deterministic normalized ties—not runtime alone.

A longer planning window can be worthwhile when reduced prefill/decode outweighs
the extra planning time, but that is an end-to-end product question. It requires
measurement on the integrated runtime and is not promised here.

## Reference points

Minimap2's anchor-chaining paper and `lchain.c` are useful precedent for implicit
predecessor DP, backpointers and bounded predecessor scans:

- <https://doi.org/10.1093/bioinformatics/bty191>
- <https://github.com/lh3/minimap2/blob/master/lchain.c>

They are references, not drop-in implementations: Minimap2 uses different gap
scoring, pruning and bounded search policies, and its RMQ variant is specific to
its objective. No Minimap2 code has been copied, and no licensing conclusion is
implied.

`petgraph` is an optional general graph toolkit, but this compatibility graph is
implicit and sparse in representation terms; materializing every compatible edge
would add overhead without supplying the specialized coalescing DP. It is not a
dependency recommendation.

## Potential directions

### Exact predecessor acceleration

For a simpler weighted two-dimensional chain, a sweep by source coordinate with
coordinate compression and Fenwick/segment-tree range maxima can answer
predecessor dominance queries without scanning every earlier candidate. A
separate query must handle the exact paired endpoint that merges adjacent ranges;
the broader compatible region increments the coalesced-group count, and `K`
becomes a DP layer.

This is design inference, not a proven drop-in result. A rough
`O(K N log N)` query/update target is plausible, but sorting, tie comparison,
backpointer reconstruction, incumbent maintenance and cancellation may dominate
or complicate the full algorithm. Equal starts need batching so invalid
same-coordinate chains are not created. Any prototype must preserve legality,
the final-range objective, deterministic normalized output, deadline behavior
and cancellation; asymptotic improvement alone is not evidence of better plans
or lower end-to-end latency.

### Quality-under-deadline search strategies

Useful-incumbent-first ordering or a legal greedy seed are hypotheses for
improving the plan available at an early deadline. They must still produce only
valid plans and must not silently replace the agreed quality hierarchy. Their
benefit is empirical: compare the best plans they return against the current
search at the same deadline, including adversarial ties and adjacency cases. No
quality guarantee is implied until such comparison exists.

Parallel candidate evaluation is a separate future option, not a mandated
framework. Shared incumbent selection, deterministic ties and cancellation would
need an explicit design before parallel work could preserve current outcomes.

## Evaluation trigger and design

Revisit the algorithm only after representative evidence, not from asymptotic
intuition alone. A useful comparison should:

- cover realistic and adversarial candidate distributions: counts, overlap,
  adjacency density, equal scores and tie-heavy pools;
- use equal deadlines, fixed hardware and a repeatable corpus, with repeated
  runs rather than one timing sample;
- record covered tokens, final coalesced range count, plan identity, and whether
  the deadline, completion or cancellation ended the search;
- keep small exhaustive-oracle cases as correctness checks for legality,
  coalescing, `K`, deterministic ties and the return contract;
- separately measure discovery quality (which candidates exist) from search
  quality (which legal plan is chosen), so a search change is not credited for
  better inputs;
- when the native integration exists, measure end-to-end planning plus
  prefill/decode rather than assuming token coverage alone predicts latency; and
- retain deadline/cancellation/valid-incumbent correctness gates. The historical
  mid-row bug—returning 6 after a computed 10-token improvement—is a behavioral
  regression guard, not a session ledger.

Quality-versus-time checkpoints can live in a future offline test harness that
calls the public API with different deadlines. This does not request a production
observer, progress callback or timeline stream. No benchmark has been run for
this note.

## Testing research note

`std::time::Instant` with a private injected clock and a background deadline
owner remains appropriate for this synchronous deadline API:

- <https://doc.rust-lang.org/std/time/struct.Instant.html>

Exact timer/flag boundary state tests remain beside the private deadline owner;
public phase tests use the opt-in mock guard's explicit simulated expiry rather
than real-time deadlines. Tokio's paused time virtualizes Tokio timers, not code reading
`std::time::Instant::now()`:

- <https://docs.rs/tokio/latest/tokio/time/fn.pause.html>

The current fake clock's stop point is coupled to checkpoint read order, which is
a known test-brittleness limitation rather than a semantic clock abstraction.
`mock_instant` is not a production dependency; the opt-in `test-support`
feature now uses it for a per-thread public test clock, while default builds and
unarmed threads continue to read `std::time::Instant`. Exact phase targeting
remains coupled to cancellation-poll order.

## Decision

Keep the current DP and deadline semantics until representative quality-under-
deadline evidence shows a concrete gap. Any future alternative must win on plan
quality at equal budget, not merely on search speed.
