#![doc = include_str!("../README.md")]
#![doc = include_str!("../docs/current-api.md")]

//! Bounded single-threaded selector for Suffix Cache Reuse.
//!
//! [`CacheReuseAnalysis::analyze`] validates caller-supplied candidate ranges,
//! searches under one absolute deadline, and returns an owned [`ReusePlan`] or
//! an informative [`PlanError`]. Discovery is not implemented yet; callers
//! currently supply candidates directly.

mod cache_reuse_analysis;
mod candidate;
mod candidate_search;
mod error;
mod reuse_plan;
mod timing;

pub use cache_reuse_analysis::CacheReuseAnalysis;
pub use candidate::{
    CandidateRange, MAX_CANDIDATES, MAX_RANGES, MAX_SCRATCH_BYTES, STACK_RESERVE_BYTES,
    ValidatedPool, validate_pool,
};
pub use candidate_search::scratch_bytes;
pub use error::{ContractError, PlanError, PlanGeometryError};
pub use reuse_plan::{ReusePlan, validate_plan};
pub use timing::{AnalysisStage, AnalysisTiming};
