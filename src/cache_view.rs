//! Borrowed actual-token cache view and caller binding identity.

/// Caller-supplied opaque binding metadata for a cache view.
///
/// This value is round-tripped on returned plans so a later native adapter can
/// correlate a proposal with the snapshot it supplied. It is not a staleness
/// check, token-identity proof, or execution authorization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CacheIdentity(u64);

impl CacheIdentity {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u64 {
        self.0
    }
}

/// One borrowed, eligible actual-token cache source.
///
/// `tokens` contains the actual token IDs represented by cached positions. Any
/// historical artificial/intentional token is ordinary source content; the
/// planner has no source layout/provenance and never classifies token values as
/// padding. `minimum_match_tokens` is the smallest exact contiguous match that
/// may become a candidate. It must be greater than zero; a minimum longer than
/// an input simply means there can be no qualifying match.
///
/// Native integration still owns source pinning, physical positions, generation
/// checks and execution. The identity is only binding metadata carried through
/// the plan.
#[derive(Debug, Clone, Copy)]
pub struct CacheView<'a> {
    pub tokens: &'a [u32],
    pub identity: CacheIdentity,
    pub minimum_match_tokens: u32,
}
