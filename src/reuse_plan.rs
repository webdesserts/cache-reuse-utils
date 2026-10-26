//! Returned reuse plans and selected-plan geometry validation.

use crate::cache_view::CacheIdentity;
use crate::candidate::{CandidateRange, MAX_RANGES};
use crate::discovery::DiscoveryStatus;
use crate::error::PlanGeometryError;
use crate::placement::{PlacementMetrics, PlacementPolicy, PlacementRecipe, PlacementStatus};
use crate::placement_search::PlacementOutcome;
use crate::timing::AnalysisTiming;

/// A selected reuse plan owned by the caller-facing result.
///
/// An empty plan is a deliberate complete no-reuse result. A non-empty plan
/// carries logical ranges, the prepared placement recipe and the status that
/// says how much exploration completed. The plan is stateless: it owns copied
/// coordinates and metadata, not a native cache handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReusePlan {
    ranges: Vec<CandidateRange>,
    covered_tokens: u64,
    timing: AnalysisTiming,
    cache_identity: CacheIdentity,
    discovery_status: DiscoveryStatus,
    placement: PlacementRecipe,
    metrics: PlacementMetrics,
    placement_status: PlacementStatus,
}

impl ReusePlan {
    pub(crate) fn from_outcome(
        context_len: usize,
        k: u32,
        timing: AnalysisTiming,
        cache_identity: CacheIdentity,
        discovery_status: DiscoveryStatus,
        outcome: PlacementOutcome,
        policy: &PlacementPolicy<'_>,
    ) -> Result<Self, PlanGeometryError> {
        let ranges: Vec<CandidateRange> = outcome
            .plan
            .iter()
            .map(|range| CandidateRange {
                source_start: range.source_start,
                destination_start: range.destination_start,
                token_count: range.token_count,
            })
            .collect();
        validate_plan(&ranges, k)?;
        let covered_tokens = outcome.covered;
        let prepared_tokens = (context_len as u64)
            .checked_add(outcome.padding as u64)
            .ok_or(PlanGeometryError::OffsetOverflow { index: 0 })?;
        let prepared_tokens = u32::try_from(prepared_tokens)
            .map_err(|_| PlanGeometryError::OffsetOverflow { index: 0 })?;
        let fresh_tokens = (context_len as u64)
            .checked_sub(covered_tokens)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or(PlanGeometryError::OffsetOverflow { index: 0 })?;
        let allowance = policy.loss_allowance;
        let admissible_min = outcome.best_useful
            - outcome
                .best_useful
                .saturating_mul(allowance.basis_points() as u64)
                / 10_000;
        let metrics = PlacementMetrics {
            useful_tokens: covered_tokens,
            inserted_padding: outcome.padding,
            prepared_tokens,
            fresh_tokens,
            best_useful: outcome.best_useful,
            admissible_min,
            loss_allowance: allowance,
        };
        let placement = PlacementRecipe {
            cache_identity,
            original_tokens: context_len as u32,
            prepared_tokens,
            ranges: outcome.plan,
            insertions: outcome.insertions,
        };
        placement
            .validate()
            .map_err(|_| PlanGeometryError::InvalidPlacementRecipe)?;
        Ok(Self {
            ranges,
            covered_tokens,
            timing,
            cache_identity,
            discovery_status,
            placement,
            metrics,
            placement_status: outcome.status,
        })
    }

    /// Final chosen ranges in original target/source coordinates.
    pub fn ranges(&self) -> &[CandidateRange] {
        &self.ranges
    }

    /// Number of original target tokens covered by reused ranges.
    pub fn covered_tokens(&self) -> u64 {
        self.covered_tokens
    }

    /// Cooperative stage timing for this analysis attempt.
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

    /// Exact source/prepared ranges and this call's insertion recipe. This is
    /// always present, including for an empty or deliberately no-pad plan.
    pub fn placement(&self) -> &PlacementRecipe {
        &self.placement
    }

    /// Placement accounting and the useful-coverage floor used to select it.
    pub fn metrics(&self) -> PlacementMetrics {
        self.metrics
    }

    /// Whether exploration completed or stopped at a declared bound.
    pub fn placement_status(&self) -> PlacementStatus {
        self.placement_status
    }
}

/// Validate the selected-plan geometry produced by search.
///
/// Rules: at most `K` ranges; ranges are ordered by destination then source;
/// each is non-empty and representable without offset overflow; consecutive
/// ranges do not cross or overlap on either axis; touching ranges are rejected
/// because the selector must emit coalesced final ranges. An empty plan is
/// valid (explicit no-reuse output). This checks plan structure only: it has no
/// extent parameters and does not prove token equality or native safety.
///
/// This is internal algorithm validation; caller-facing recipe validation is
/// `PlacementRecipe::validate`.
pub(crate) fn validate_plan(plan: &[CandidateRange], k: u32) -> Result<(), PlanGeometryError> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn range(source_start: u32, destination_start: u32, token_count: u32) -> CandidateRange {
        CandidateRange {
            source_start,
            destination_start,
            token_count,
        }
    }

    #[test]
    fn selected_plan_geometry_requires_order_disjointness_and_coalescing() {
        assert!(validate_plan(&[], 2).is_ok());
        assert!(validate_plan(&[range(0, 0, 4), range(8, 12, 3)], 2).is_ok());
        assert!(matches!(
            validate_plan(&[range(10, 0, 2), range(0, 3, 2)], 2),
            Err(PlanGeometryError::CrossingOrOverlap { index: 1 })
        ));
        assert!(matches!(
            validate_plan(&[range(10, 0, 2), range(3, 0, 2)], 2),
            Err(PlanGeometryError::Misordered { index: 1 })
        ));
        assert!(matches!(
            validate_plan(&[range(0, 0, 4), range(4, 4, 2)], 2),
            Err(PlanGeometryError::AdjacentRanges { index: 1 })
        ));
        assert!(matches!(
            validate_plan(&[range(0, 0, 1), range(2, 2, 1)], 1),
            Err(PlanGeometryError::TooManyRanges { got: 2, max: 1 })
        ));
        assert!(matches!(
            validate_plan(&[range(0, 0, 0)], 1),
            Err(PlanGeometryError::InvalidRange { index: 0 })
        ));
        assert!(matches!(
            validate_plan(&[range(u32::MAX, 0, 1)], 1),
            Err(PlanGeometryError::OffsetOverflow { index: 0 })
        ));
    }
}
