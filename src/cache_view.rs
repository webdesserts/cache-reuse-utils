//! Borrowed actual-token cache view and caller binding identity.

/// Caller-supplied opaque binding metadata for a cache view.
///
/// This value is round-tripped on returned plans so a later native adapter can
/// correlate a proposal with the snapshot it supplied. It is not a staleness
/// check, token-identity proof, or execution authorization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CacheIdentity(u64);

impl CacheIdentity {
    /// Create an opaque caller value. The planner only round-trips it.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Return the original caller value.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// One borrowed cache source of actual token IDs.
///
/// Every token is ordinary content; the planner never treats a token value as
/// padding. The engine adapter still owns pinning, physical positions and
/// generation checks.
#[derive(Debug, Clone, Copy)]
pub struct CacheView<'a> {
    /// Actual token IDs in cached positions. This slice is borrowed only for
    /// the synchronous analysis call.
    pub tokens: &'a [u32],
    /// Opaque binding metadata copied to every returned plan.
    pub identity: CacheIdentity,
    /// Smallest qualifying exact contiguous match, in tokens. Must be nonzero;
    /// a value longer than either input means no candidate can match.
    pub minimum_match_tokens: u32,
}
