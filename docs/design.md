# Design notes

How the planner finds and chooses a plan, and what bounds its work.

## Discovery

The planner borrows one cache of actual token IDs and the target context. It
finds maximal exact matches at least `minimum_match_tokens` long inside the
reusable window, between the protected prefix and the fresh suffix; a match
that crosses either edge is clipped rather than dropped. Each cache window is
hashed once and sorted. Every target window looks up equal hashes and verifies
the tokens, so a hash collision never becomes a match. A per-diagonal end
marker skips windows inside a match already found.

A bounded heap keeps the 4,096 longest matches, preferring earlier destination
and source positions on ties. A long match found late is therefore never
displaced by many short early ones. Dropping any match sets
`DiscoveryStatus::CandidateLimitReached`.

Historical pad tokens are ordinary content. Only this call's insertions are
padding.

## Search

Each match yields an untrimmed variant, then any extra trims the policy asks
for, up to `max_variants`. Variants are searched in destination order. Placing
a variant after a previous range left-trims it just enough to start after that
range in both target and source, then to source-start and count alignment. So
overlapping matches combine without a trim allowance. The placement then takes
the smallest padding that meets prepared-start alignment and the minimum
prepared start, inserted at the latest allowed boundary in the gap. Ranges that
continue each other on one diagonal coalesce and use one of the `k` slots.

Every partial plan is a label in an arena: the placed piece, a parent index
and running totals. Labels are deduplicated per variant by
`(ranges, padding, end)`, keeping the best coverage, because nothing else
affects how a plan can be extended. The winning plan is rebuilt once from its
parent chain.

Before that search, a greedy pass adds variants longest first while coverage
improves. A deadline or cap therefore still returns a plan built from the
longest matches, not just the leftmost ones.

## Selection

Let `B` be the best coverage found. Plans covering at least
`B - floor(B * loss_basis_points / 10000)` are eligible. Among them the planner
maximizes `covered - padding * pad_penalty_tokens`, then prefers less padding,
more coverage and fewer ranges. When the floor is zero and the winner's
coverage does not exceed its pad cost, it returns the empty plan instead.

## Bounds

- Inputs are capped at 2^20 tokens per side.
- Discovery keeps at most 4,096 matches, and derivation at most `max_variants`
  (default 4,096) variants.
- Search keeps at most 65,536 labels and stops with `StateLimitReached` beyond
  that.
- Deadline and cancellation checks run at stage boundaries and every 4,096
  inner-loop steps. There is no timer thread. Cancellation is checked first, so
  it always wins.

Search over all label pairs is quadratic in the worst case. At 4,000 variants
and `k = 10` a full search takes about a second in release builds. The deadline
bounds it, and the greedy incumbent gives a strong plan early.

## Validity boundary

A plan is a structural proposal. The engine adapter must still check tokens,
bounds, alignments and cache generations against native state before using it.
