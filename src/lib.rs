#![doc = include_str!("../README.md")]
#![doc = include_str!("../docs/current-api.md")]

//! Bounded single-threaded selector for Suffix Cache Reuse.
//!
//! [`CacheReuseAnalysis::analyze`] discovers exact maximal matches in a borrowed
//! actual-token cache view, searches declared placement variants under one
//! absolute deadline, and returns an owned [`ReusePlan`] with a placement recipe
//! or an informative [`PlanError`]. Native pinning, physical mapping and
//! execution remain adapter work.

mod cache_reuse_analysis;
mod cache_view;
mod candidate;
mod deadline;
mod discovery;
mod error;
mod placement;
mod placement_search;
mod reuse_plan;
mod timing;

#[cfg(feature = "test-support")]
pub mod test_support;

pub use cache_reuse_analysis::CacheReuseAnalysis;
pub use cache_view::{CacheIdentity, CacheView};
pub use candidate::{
    CandidateRange, MAX_CANDIDATES, MAX_RANGES, MAX_SCRATCH_BYTES, STACK_RESERVE_BYTES,
    ValidatedPool, validate_pool,
};
pub use discovery::DiscoveryStatus;
pub use error::{AdmissionDeniedReason, ContractError, InputSide, PlanError, PlanGeometryError};
pub use placement::{
    DEFAULT_LOSS_BASIS_POINTS, Insertion, LossAllowance, MAX_NEW_PADDING, MAX_PLACEMENT_BYTES,
    MAX_PLACEMENT_VARIANTS, PlacedRange, PlacementConfigError, PlacementMaterializeError,
    PlacementMetrics, PlacementPolicy, PlacementPosition, PlacementRecipe, PlacementStatus,
};
pub use placement_search::scratch_bytes;
pub use reuse_plan::{ReusePlan, validate_plan};
pub use timing::{AnalysisStage, AnalysisTiming};
