//! The plan returned by a successful analysis.

use crate::cache_view::CacheIdentity;
use crate::discovery::DiscoveryStatus;
use crate::placement::{
    PlacedRange, PlacementMetrics, PlacementPolicy, PlacementRecipe, PlacementStatus,
};
use crate::placement_search::PlacementOutcome;
use crate::timing::AnalysisTiming;

/// A selected reuse plan.
///
/// An empty plan is a deliberate no-reuse result. A plan owns copied
/// coordinates and metadata, never a native cache handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReusePlan {
    placement: PlacementRecipe,
    metrics: PlacementMetrics,
    placement_status: PlacementStatus,
    discovery_status: DiscoveryStatus,
    timing: AnalysisTiming,
}

impl ReusePlan {
    /// Build the plan and its recipe. Returns `None` if the recipe fails its
    /// own structural validation, which would be a planner bug.
    pub(crate) fn from_outcome(
        context_len: usize,
        cache_identity: CacheIdentity,
        discovery_status: DiscoveryStatus,
        outcome: PlacementOutcome,
        policy: &PlacementPolicy<'_>,
        timing: AnalysisTiming,
    ) -> Option<Self> {
        let original_tokens = context_len as u32;
        let prepared_tokens = original_tokens.checked_add(outcome.padding)?;
        let metrics = PlacementMetrics {
            useful_tokens: outcome.covered,
            inserted_padding: outcome.padding,
            prepared_tokens,
            fresh_tokens: original_tokens - outcome.covered as u32,
            best_useful: outcome.best_useful,
            admissible_min: policy.loss_allowance.floor(outcome.best_useful),
            loss_allowance: policy.loss_allowance,
        };
        let placement = PlacementRecipe {
            cache_identity,
            original_tokens,
            prepared_tokens,
            ranges: outcome.plan,
            insertions: outcome.insertions,
        };
        placement.validate().ok()?;
        Some(Self {
            placement,
            metrics,
            placement_status: outcome.status,
            discovery_status,
            timing,
        })
    }

    /// Final reused ranges, in source, original-target and prepared
    /// coordinates. Shorthand for `placement().ranges`.
    pub fn ranges(&self) -> &[PlacedRange] {
        &self.placement.ranges
    }

    /// Number of original target tokens covered by reused ranges.
    pub fn covered_tokens(&self) -> u64 {
        self.metrics.useful_tokens
    }

    /// Stage timing for this analysis.
    pub fn timing(&self) -> AnalysisTiming {
        self.timing
    }

    /// The caller identity from the cache view, also present on empty plans.
    /// This is correlation metadata, not proof of native freshness.
    pub fn cache_identity(&self) -> CacheIdentity {
        self.placement.cache_identity
    }

    /// Whether discovery was complete or stopped at the candidate capacity.
    pub fn discovery_status(&self) -> DiscoveryStatus {
        self.discovery_status
    }

    /// The prepared-target recipe: reused ranges and this call's insertions.
    /// Always present, including for an empty plan.
    pub fn placement(&self) -> &PlacementRecipe {
        &self.placement
    }

    /// Coverage, padding and selection-floor accounting.
    pub fn metrics(&self) -> PlacementMetrics {
        self.metrics
    }

    /// Whether search completed or stopped at a deadline or cap.
    pub fn placement_status(&self) -> PlacementStatus {
        self.placement_status
    }
}
