//! Public error types for candidate input, selected-plan geometry and analysis.

use crate::timing::{AnalysisStage, AnalysisTiming};

/// Why a candidate pool was rejected. Only arithmetic and per-candidate shape
/// are checked: candidates may compete and may appear in any order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractError {
    TooManyCandidates { got: usize, max: usize },
    InvalidK { k: u32, max: u32 },
    ZeroCount { index: usize },
    OffsetOverflow { index: usize },
    ScratchExceedsLimit { bytes: u64, max: u64 },
}

/// Why a selected plan was rejected. These are output properties: selected
/// ranges must not cross, overlap or touch (touching ranges must be coalesced).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanGeometryError {
    TooManyRanges { got: usize, max: u32 },
    InvalidRange { index: usize },
    OffsetOverflow { index: usize },
    Misordered { index: usize },
    CrossingOrOverlap { index: usize },
    AdjacentRanges { index: usize },
}

/// Distinct refusals from a planning attempt. Every variant carries the stage
/// where the attempt stopped and a small timing summary. A usable plan is never
/// carried in an error and no error authorises continuing inference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// Caller input failed validation during preparation.
    InvalidInput {
        reason: ContractError,
        stage: AnalysisStage,
        timing: AnalysisTiming,
    },
    /// The deadline stopped search before any valid plan was found.
    BudgetExhausted {
        stage: AnalysisStage,
        timing: AnalysisTiming,
    },
    /// The whole request was cancelled. Even if an incumbent existed, it must
    /// not be used to continue this request.
    Cancelled {
        stage: AnalysisStage,
        timing: AnalysisTiming,
    },
    /// Planner resource admission refused (scratch/table/plan allocation).
    /// This is distinct from native model-state or execution validation.
    AdmissionDenied {
        stage: AnalysisStage,
        timing: AnalysisTiming,
    },
    /// An internal planning invariant failed; no plan may be used.
    Invariant {
        reason: &'static str,
        stage: AnalysisStage,
        timing: AnalysisTiming,
    },
}

/// Short invariant description for a rejected selected plan.
pub(crate) fn geometry_reason(error: PlanGeometryError) -> &'static str {
    match error {
        PlanGeometryError::TooManyRanges { .. } => "selected ranges exceed K",
        PlanGeometryError::InvalidRange { .. } => "selected range is empty",
        PlanGeometryError::OffsetOverflow { .. } => "selected range overflows",
        PlanGeometryError::Misordered { .. } => "selected ranges are misordered",
        PlanGeometryError::CrossingOrOverlap { .. } => "selected ranges cross or overlap",
        PlanGeometryError::AdjacentRanges { .. } => "adjacent selected ranges were not coalesced",
    }
}
