#![doc = include_str!("../README.md")]

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
// Public exports are limited to the analysis contract and configuration that a
// caller or the C ABI adapter actually consumes. Dead raw-pool/scratch and
// standalone plan-validation facades are not part of the public contract.
pub use candidate::{CandidateRange, MAX_RANGES};
pub use discovery::DiscoveryStatus;
pub use error::{AdmissionDeniedReason, ContractError, InputSide, PlanError};
pub use placement::{
    Insertion, LossAllowance, MAX_NEW_PADDING, MAX_PLACEMENT_VARIANTS, PlacedRange,
    PlacementConfigError, PlacementMaterializeError, PlacementMetrics, PlacementPolicy,
    PlacementPosition, PlacementRecipe, PlacementStatus,
};
pub use reuse_plan::ReusePlan;
pub use timing::{AnalysisStage, AnalysisTiming};
