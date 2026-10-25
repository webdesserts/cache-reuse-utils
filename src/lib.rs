#![doc = include_str!("../README.md")]
#![doc = include_str!("../docs/current-api.md")]

//! Bounded single-threaded selector for Suffix Cache Reuse.
//!
//! [`CacheReuseAnalysis::analyze`] discovers exact maximal matches in a borrowed
//! logical cache view, searches under one absolute deadline, and returns an
//! owned [`ReusePlan`] or an informative [`PlanError`]. Native projection,
//! pinning, physical placement and execution remain adapter work.

mod cache_reuse_analysis;
mod cache_view;
mod candidate;
mod candidate_search;
mod deadline;
mod discovery;
mod error;
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
pub use candidate_search::scratch_bytes;
pub use discovery::DiscoveryStatus;
pub use error::{AdmissionDeniedReason, ContractError, InputSide, PlanError, PlanGeometryError};
pub use reuse_plan::{ReusePlan, validate_plan};
pub use timing::{AnalysisStage, AnalysisTiming};
