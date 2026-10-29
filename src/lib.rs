#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

mod cache_reuse_analysis;
mod cache_view;
mod candidate;
mod coordinates;
mod discovery;
mod error;
mod placement;
mod placement_search;
mod reuse_plan;
mod timing;

pub use cache_reuse_analysis::CacheReuseAnalysis;
pub use cache_view::{CacheIdentity, CacheView};
pub use candidate::MAX_RANGES;
pub use coordinates::{
    CommonPrefix, common_logical_prefix, logical_boundary, project_to_logical,
    validate_pad_positions,
};
pub use discovery::DiscoveryStatus;
pub use error::{AdmissionDeniedReason, ContractError, InputSide, PlanError, PlanErrorKind};
pub use placement::{
    AllowedInsertions, DEFAULT_PAD_PENALTY_TOKENS, DEFAULT_RANGE_PENALTY_TOKENS, Insertion,
    LossAllowance, MAX_NEW_PADDING, MAX_PLACEMENT_VARIANTS, PlacedRange, PlacementConfigError,
    PlacementMaterializeError, PlacementMetrics, PlacementPolicy, PlacementPosition,
    PlacementRecipe, PlacementStatus,
};
pub use reuse_plan::ReusePlan;
pub use timing::{AnalysisStage, AnalysisTiming};
