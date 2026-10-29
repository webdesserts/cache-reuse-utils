# Design Notes

This document explains how `cache-reuse-utils` discovers exact token matches,
searches for a high-coverage placement that satisfies hardware alignment and
padding constraints, and bounds its runtime and memory usage without keeping
state between calls.

---

## 1. Stateless Discovery (`src/discovery.rs`)

Discovery borrows one cached token slice (`source`) and the target prompt
(`context`), and finds maximal exact token matches of length at least
`minimum_match_tokens` inside the reusable target window (after
`protected_prefix_tokens` and before `fresh_suffix_tokens`). If a shared span
crosses the prefix or suffix boundary, it is clipped to the reusable window
rather than discarded.

### Single-pass chained hash index

To stay stateless while running in a few milliseconds on large contexts,
discovery avoids sorting seed arrays:

1. **Hash table construction (`O(N)`):** Every `minimum_match_tokens` window in
   the source cache is rolling-hashed in a single pass and linked into a flat
   power-of-two chained hash table (`head` and `next` index arrays). Because the
   source is inserted in reverse index order, each hash chain is naturally
   ordered by ascending `source_start`.
2. **Exact verification and diagonal skipping:** As the rolling hash walks the
   target window, each target offset traverses its matching hash chain and
   compares the window tokens directly so hash collisions never produce false
   matches. A per-diagonal watermark (`covered[source - destination + target_len]`)
   tracks the furthest target index already covered by a match on that
   diagonal, skipping interior windows of an already-discovered match in `O(1)`.
3. **Run-skipping extension:** Both inputs precompute contiguous single-token
   run bounds (`run_start` and `run_end`). When extending a verified seed left
   and right, the matcher jumps across shared single-token runs in a single
   step rather than advancing one token at a time.

### Bounding repetitive inputs

Pathological or highly repetitive inputs (such as 100,000 identical tokens or
repeated boilerplate) create long hash chains:

- **Per-window probe caps:** Each target window probes at most `256` equal-hash
  source windows (`MAX_BUCKET_PROBES`). Inside a single-token run of at least
  `64` identical tokens (`LONG_RUN`), where interior positions mostly extend
  matches already found at the start of the run, the cap tightens to `16`
  probes (`MAX_RUN_PROBES`). Whenever probes are skipped, discovery sets
  `DiscoveryStatus::CandidateLimitReached`. Because matches extend both left
  and right, long spans that contain any lower-frequency window are still
  discovered in full.
- **Early length pruning:** Once the candidate heap is full, any `(source, target)`
  window pair whose maximum geometric extension to the boundaries of the input
  cannot beat the weakest match currently in the heap is skipped before
  extension.

### Retaining the 4,096 longest matches

Discovery maintains a bounded min-heap of the `4,096` (`MAX_CANDIDATES`)
longest maximal matches, breaking ties in favor of earlier `destination_start`
and then earlier `source_start`. Retaining the longest matches rather than the
first 4,096 matches ensures that a long match near the end of a prompt is never
crowded out by short matches near the beginning.

All tokens in the cache and target are treated as ordinary content; only tokens
inserted by the current planning call count as padding.

---

## 2. Placement Search (`src/placement_search.rs`)

Given the discovered matches, placement search chooses up to `k` (`<= 10`)
monotonically increasing ranges in both source and target coordinates that
satisfy the caller's `PlacementPolicy`.

### Step 1: Variant derivation and destination-end ordering

Each maximal match produces its untrimmed span first, followed by any extra
left/right trims requested via `max_trim_left` and `max_trim_right`, up to
`max_variants` (default `4,096`).

Derived variants are sorted by **ascending `destination_end`** (then
`destination_start` and `source_start`). Ordering by `destination_end` is
essential for correctness when matches overlap: a shorter match that starts and
ends inside a longer match's span can legally precede the left-trimmed
remainder of that longer match. Because left-trimming a variant changes its
start without changing its end, sorting by `destination_end` guarantees that
every valid predecessor is visited before the variant that extends it.

### Step 2: Greedy incumbent seeding

Before running the full dynamic program, `seed_greedy` considers variants from
longest to shortest and greedily adds each variant that improves total
coverage. Seeding this incumbent into the label arena guarantees that if the
deadline or state limit interrupts search early, the planner still returns a
plan built from the longest matches across the whole prompt rather than only
the leftmost matches visited so far.

### Step 3: Fitting, padding, and alignment left-trimming

When placing a variant after a predecessor range, `fit()` first left-trims the
variant just enough to start at or after the predecessor's `destination_end`
and `source_end`, and then rounds its start and length to satisfy
`source_start_alignment` and `reuse_count_alignment`. Overlapping matches can
therefore chain together automatically without requiring `max_trim_left`.

Next, the planner checks whether the piece's prepared target start
(`destination_start + prior_padding`) satisfies `prepared_target_start_alignment`
and `minimum_prepared_reuse_start`. When it does not, `step()` evaluates up to
**two** candidate placements for that variant:

1. **Padded placement:** Insert the smallest number of `pad_token`s needed to
   reach the next aligned prepared start, placed at the latest allowed
   insertion boundary in the gap between the previous range and this piece.
2. **Alignment left-trimmed placement (zero padding):** Leave the first few
   tokens of the match to be computed fresh as normal prefill so the reused
   slice starts directly on an aligned prepared boundary without inserting any
   padding. Because advancing `destination_start` also advances `source_start`
   (which may require re-rounding to `source_start_alignment`), `trim_to_aligned`
   iterates up to `64` times until both source and prepared-target alignments
   agree.

If a zero-pad piece immediately follows the previous range in both source and
target coordinates (`after == (destination_start, source_start)`), the two
pieces coalesce into a single contiguous range and consume only one of the `k`
range slots.

### Step 4: Arena dynamic programming and the `O(log² V)` predecessor index

Every reachable partial plan is stored as a `Label` in a flat arena with a
parent pointer (`parent: u32`) and cumulative totals (`ranges`, `padding`,
`covered`). Within each variant step, candidate labels are deduplicated by
`(ranges, padding, destination_end, source_end)`, keeping the highest coverage,
since those four values completely determine how a partial plan can be extended
by later variants.

Scanning all previous labels for every variant would take `O(V · L)` time.
Instead, `src/placement_search/index.rs` separates predecessors into two
cases:

1. **Clear (non-overlapping) predecessors (`prev.destination_end <= start.destination_start` and `prev.source_end <= start.source_start`):**
   Because a clear predecessor does not force `fit()` to trim the current
   variant, every clear predecessor in the same `(ranges, padding)` group
   produces the exact same placed piece. Only the predecessor with the highest
   `covered` total in that 2D prefix region matters.
   `Index::best_clear` answers that 2D prefix-maximum query in **`O(log² V)`**
   time per `(ranges, padding)` group using a Fenwick tree over the sorted axis
   of variant `source_start`s, where each Fenwick node maintains a Pareto
   staircase of `(destination_end, covered, label)`.
2. **Overlapping or adjacent predecessors:**
   When a prior label ends inside the current variant's target or source span,
   the amount trimmed from the current variant depends on the predecessor's
   exact end coordinate. The index stores label IDs in `BTreeMap`s keyed by
   `destination_end` and `source_end` and visits only the labels whose ends fall
   inside the variant's span.

---

## 3. Plan Selection

Let `B` be the maximum raw token coverage (`best_useful`) achieved by any
completed label. Every label covering at least
`B - floor(B * loss_basis_points / 10_000)` tokens is eligible.

Among eligible labels, `select()` ranks plans lexicographically by:

1. **Highest net coverage:** `covered - padding * pad_penalty_tokens`
2. **Fewest inserted pad tokens:** smaller `padding`
3. **Highest raw coverage:** larger `covered`
4. **Fewest ranges:** smaller `ranges`

When `loss_allowance` is 100% (`admissible_min == 0`) and the winning plan's
raw coverage does not exceed its pad penalty cost, the planner returns a
deliberate empty plan rather than a net-negative padded plan. Finally, only the
winning label's parent chain is walked to reconstruct the `Vec<PlacedRange>`
and `Vec<Insertion>`.

---

## 4. Resource Bounds and Performance

- **Input size:** Up to `1,048,576` (`2^20`) tokens per side.
- **Candidate and variant limits:** Discovery retains at most `4,096` maximal
  matches; variant derivation produces at most `max_variants` (default `4,096`)
  variants.
- **Search state and index limits:** Search retains at most `65,536` (`2^16`)
  labels and caps total Fenwick-tree nodes at `2,097,152` (`2^21`), stopping
  with `PlacementStatus::StateLimitReached` if either bound is reached.
- **Cooperative stopping:** `Stopper` checks cancellation and the deadline at
  every stage boundary and once every `4,096` inner-loop ticks, with no
  background timer threads.

In release builds on Apple Silicon, a full search over `4,000` non-overlapping
variants with `k = 10` completes in under `10 ms`, a `64k`-token edited-prompt
analysis completes in `~1.2 ms`, and a `1M`-token edited-prompt analysis
completes in `~29 ms`.

---

## 5. Validity Boundary

A `ReusePlan` is a stateless structural proposal. Before executing a plan, the
host engine adapter must still verify token identity, block alignment, and KV
cache generation validity against its live native state.
