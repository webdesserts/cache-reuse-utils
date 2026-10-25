//! Returned reuse plans and selected-plan geometry validation.

use crate::cache_view::CacheIdentity;
use crate::candidate::{CandidateRange, MAX_RANGES};
use crate::discovery::DiscoveryStatus;
use crate::error::PlanGeometryError;
use crate::timing::AnalysisTiming;

/// A selected reuse plan owned by the caller-facing result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReusePlan {
    ranges: Vec<CandidateRange>,
    covered_tokens: u64,
    timing: AnalysisTiming,
    cache_identity: CacheIdentity,
    discovery_status: DiscoveryStatus,
}

impl ReusePlan {
    pub(crate) fn from_parts(
        ranges: Vec<CandidateRange>,
        k: u32,
        timing: AnalysisTiming,
        cache_identity: CacheIdentity,
        discovery_status: DiscoveryStatus,
    ) -> Result<Self, PlanGeometryError> {
        validate_plan(&ranges, k)?;
        let covered_tokens = ranges.iter().map(|range| range.token_count as u64).sum();
        Ok(Self {
            ranges,
            covered_tokens,
            timing,
            cache_identity,
            discovery_status,
        })
    }

    pub fn ranges(&self) -> &[CandidateRange] {
        &self.ranges
    }

    pub fn covered_tokens(&self) -> u64 {
        self.covered_tokens
    }

    pub fn timing(&self) -> AnalysisTiming {
        self.timing
    }

    /// Caller identity round-tripped from the cache view, including for an
    /// empty plan. This is binding metadata, not native staleness proof.
    pub fn cache_identity(&self) -> CacheIdentity {
        self.cache_identity
    }

    /// Whether discovery was complete or stopped at the candidate capacity.
    pub fn discovery_status(&self) -> DiscoveryStatus {
        self.discovery_status
    }
}

/// Validate a selected plan independently of how completely it was searched.
///
/// Rules: at most `K` ranges; ranges are ordered by destination then source;
/// each is non-empty and representable without offset overflow; consecutive
/// ranges do not cross or overlap on either axis; touching ranges are rejected
/// because the selector must emit coalesced final ranges. An empty plan is
/// valid (explicit no-reuse output). This checks plan structure only: it has no
/// extent parameters and does not prove token equality or native safety.
pub fn validate_plan(plan: &[CandidateRange], k: u32) -> Result<(), PlanGeometryError> {
    if k == 0 || k > MAX_RANGES {
        return Err(PlanGeometryError::TooManyRanges {
            got: plan.len(),
            max: k.min(MAX_RANGES),
        });
    }
    if plan.len() > k as usize {
        return Err(PlanGeometryError::TooManyRanges {
            got: plan.len(),
            max: k,
        });
    }
    let mut previous: Option<&CandidateRange> = None;
    for (index, range) in plan.iter().enumerate() {
        if range.token_count == 0 {
            return Err(PlanGeometryError::InvalidRange { index });
        }
        let source_end = range
            .source_end()
            .ok_or(PlanGeometryError::OffsetOverflow { index })?;
        let destination_end = range
            .destination_end()
            .ok_or(PlanGeometryError::OffsetOverflow { index })?;
        if let Some(previous) = previous {
            let previous_source_end = previous
                .source_end()
                .ok_or(PlanGeometryError::OffsetOverflow { index: index - 1 })?;
            let previous_destination_end = previous
                .destination_end()
                .ok_or(PlanGeometryError::OffsetOverflow { index: index - 1 })?;
            if (previous.destination_start, previous.source_start)
                > (range.destination_start, range.source_start)
            {
                return Err(PlanGeometryError::Misordered { index });
            }
            if previous_source_end > range.source_start
                || previous_destination_end > range.destination_start
            {
                return Err(PlanGeometryError::CrossingOrOverlap { index });
            }
            if previous_source_end == range.source_start
                && previous_destination_end == range.destination_start
            {
                return Err(PlanGeometryError::AdjacentRanges { index });
            }
        }
        let _ = (source_end, destination_end);
        previous = Some(range);
    }
    Ok(())
}
