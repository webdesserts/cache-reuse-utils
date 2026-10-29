//! Candidate ranges and planner admission bounds.

/// Maximum number of final reused ranges a plan may contain (the largest
/// accepted `k`).
pub const MAX_RANGES: u32 = 10;
/// Maximum number of discovered candidate matches retained for search.
pub(crate) const MAX_CANDIDATES: usize = 4096;

/// One reuse candidate in logical token coordinates.
///
/// Both sequences are measured in tokens, not bytes or positions. The range is
/// half-open: it covers `token_count` tokens starting at `source_start` in the
/// retained source and at `destination_start` in the destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CandidateRange {
    /// First reused token in the retained source sequence.
    pub source_start: u32,
    /// First replaced token in the destination sequence.
    pub destination_start: u32,
    /// Number of tokens in the range; must be greater than zero.
    pub token_count: u32,
}

impl CandidateRange {
    pub fn source_end(&self) -> Option<u32> {
        self.source_start.checked_add(self.token_count)
    }

    pub fn destination_end(&self) -> Option<u32> {
        self.destination_start.checked_add(self.token_count)
    }
}
