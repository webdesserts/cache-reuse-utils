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
mod pool_select;
mod reconcile;
mod reuse_plan;
mod schedule;
mod separators;
mod timing;

pub use cache_reuse_analysis::CacheReuseAnalysis;
pub use cache_view::{CacheIdentity, CacheView};
pub use candidate::MAX_RANGES;
pub use coordinates::{
    CommonPrefix, CoordinateMap, append_unpadded, common_logical_prefix, logical_boundary,
    project_to_logical, slice_pad_positions, truncate_pad_positions, validate_pad_positions,
    verify_matches_logical, verify_span_correspondence,
};
pub use discovery::DiscoveryStatus;
pub use error::{AdmissionDeniedReason, ContractError, InputSide, PlanError, PlanErrorKind};
pub use placement::{
    AllowedInsertions, DEFAULT_PAD_PENALTY_TOKENS, DEFAULT_RANGE_PENALTY_TOKENS, Insertion,
    LossAllowance, MAX_NEW_PADDING, MAX_PLACEMENT_VARIANTS, PlacedRange, PlacementConfigError,
    PlacementMaterializeError, PlacementMetrics, PlacementPolicy, PlacementPosition,
    PlacementRecipe, PlacementStatus,
};
pub use pool_select::{
    CandidateAction, CandidateInput, CandidateSelection, CandidateSelectionPolicy, score_candidate,
    select_candidate,
};
pub use reconcile::{ExecutionSummary, clip_ranges_to_archive, reconcile_executed_ranges};
pub use reuse_plan::ReusePlan;
pub use schedule::{ArchiveGeometry, ArchiveOp, compile_archive_schedule, verify_archive_schedule};
pub use separators::{SeparatorPolicy, collect_separator_boundaries, verify_separator_map};
pub use timing::{AnalysisStage, AnalysisTiming};
