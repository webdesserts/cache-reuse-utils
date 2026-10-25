//! Candidate ranges, pool constants and input validation.

use crate::candidate_search::scratch_bytes;
use crate::error::ContractError;

/// Maximum selected chain slots (`DS4_SCR_MULTI_MAX_RANGES`).
pub const MAX_RANGES: u32 = 6;
/// Maximum candidate pool size (`DS4_SCR_DENSE_RANGES_MAX`).
pub const MAX_CANDIDATES: usize = 4096;
/// Reference ceiling for the modeled scratch payload (`DS41_COVERAGE_BYTES_MAX`).
pub const MAX_SCRATCH_BYTES: u64 = 1024 * 1024;
/// Stack reserve added to admitted scratch (`DS41_COVERAGE_STACK_RESERVE`).
pub const STACK_RESERVE_BYTES: u64 = 4096;

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

/// A validated candidate pool. It proves arithmetic and per-candidate shape
/// only; it does not prove token equality, source extents or model-state
/// equivalence. Discovery and native validation own those obligations.
#[derive(Debug, Clone, Copy)]
pub struct ValidatedPool<'a> {
    candidates: &'a [CandidateRange],
    k: u32,
    scratch_bytes: u64,
    reserved_bytes: u64,
}

impl<'a> ValidatedPool<'a> {
    pub fn candidates(&self) -> &'a [CandidateRange] {
        self.candidates
    }

    pub fn k(&self) -> u32 {
        self.k
    }

    /// Modeled DP/table/plan payload bytes, excluding the stack reserve and
    /// allocator or spare `Vec` capacity overhead.
    pub fn scratch_bytes(&self) -> u64 {
        self.scratch_bytes
    }

    /// Scratch plus stack reserve; native admits this before allocation.
    pub fn reserved_bytes(&self) -> u64 {
        self.reserved_bytes
    }
}

/// Validate a candidate pool. Order is not significant and alternatives may
/// conflict; the selector chooses between them later.
pub fn validate_pool(
    candidates: &[CandidateRange],
    k: u32,
) -> Result<ValidatedPool<'_>, ContractError> {
    if candidates.len() > MAX_CANDIDATES {
        return Err(ContractError::TooManyCandidates {
            got: candidates.len(),
            max: MAX_CANDIDATES,
        });
    }
    if k == 0 || k > MAX_RANGES {
        return Err(ContractError::InvalidK { k, max: MAX_RANGES });
    }
    for (index, range) in candidates.iter().enumerate() {
        if range.token_count == 0 {
            return Err(ContractError::ZeroCount { index });
        }
        if range.source_end().is_none() || range.destination_end().is_none() {
            return Err(ContractError::OffsetOverflow { index });
        }
    }
    let bytes = scratch_bytes(candidates.len(), k).ok_or(ContractError::ScratchExceedsLimit {
        bytes: u64::MAX,
        max: MAX_SCRATCH_BYTES,
    })?;
    if bytes > MAX_SCRATCH_BYTES {
        return Err(ContractError::ScratchExceedsLimit {
            bytes,
            max: MAX_SCRATCH_BYTES,
        });
    }
    Ok(ValidatedPool {
        candidates,
        k,
        scratch_bytes: bytes,
        reserved_bytes: bytes + STACK_RESERVE_BYTES,
    })
}
