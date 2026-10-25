# Future dream API: native integration boundary

`CacheReuseAnalysis::analyze` now performs full Rust discovery over a borrowed
logical cache view, followed by the existing search. What remains future is the
native adapter: projecting pads, pinning a cache snapshot, translating logical
positions to physical placement, validating generation/model state and executing
the proposal.

The current implemented call is:

```rust,ignore
let mut cancel = || false;
let result = CacheReuseAnalysis {
    context_tokens,
    cache: CacheView {
        tokens: logical_cache_tokens,
        identity: CacheIdentity::new(caller_identity),
        minimum_match_tokens: 128,
    },
    k: 6,
    deadline: Instant::now() + Duration::from_secs(5),
    cancel: &mut cancel,
}.analyze();
```

`CacheView` is one borrowed eligible logical source. `CacheIdentity` is opaque
caller binding metadata and is round-tripped by the plan; it is not proof of
cache freshness, token identity or executability. Native code still owns the
source snapshot, pads, physical positions, model/cache generations, placement,
allocation and execution.

The future native path is expected to:

1. Pin one eligible sealed source and reject use while it can retire or rebase.
2. Project pads to logical tokens and supply a logical `CacheView` plus identity.
3. Run this crate's discovery/search; receive a logical `ReusePlan`.
4. Map logical ranges to native physical positions and revalidate token identity,
   bounds, alignment, pad correspondence, generation and execution eligibility.
5. Execute only if native validation succeeds; otherwise fall back without
   treating the Rust plan or identity as authorization.

Open native questions are the exact FFI token/view copy boundary, physical
position mapping ownership, and how a rejected proposal is reported alongside
the native receipt. They are not implemented here. This document does not promise
that logical token equality implies model-state equivalence or safe reuse.
