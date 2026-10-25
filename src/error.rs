//! Public error types for candidate input, selected-plan geometry and analysis.

use crate::candidate_search::SearchError;
use crate::discovery::DiscoveryError;
use crate::timing::{AnalysisStage, AnalysisTiming};

/// Why a candidate pool or analysis input was rejected. Only arithmetic and
/// per-input shape are checked; candidates may compete and may appear in any
/// order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractError {
    TooManyCandidates {
        got: usize,
        max: usize,
    },
    InvalidK {
        k: u32,
        max: u32,
    },
    /// A cache view's qualifying-match minimum must be greater than zero.
    InvalidMinimum {
        minimum: u32,
    },
    ZeroCount {
        index: usize,
    },
    OffsetOverflow {
        index: usize,
    },
    ScratchExceedsLimit {
        bytes: u64,
        max: u64,
    },
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

/// Which input side exceeded discovery's supported envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputSide {
    Context,
    Cache,
}

/// Small typed reason for a planner admission refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionDeniedReason {
    InputTooLarge {
        side: InputSide,
        got: usize,
        max: usize,
    },
    DiscoveryBuffer {
        bytes: u64,
        max: u64,
    },
    /// A bounded planner allocation was refused by the allocator.
    Allocation,
    /// The background deadline owner could not be started or its lock was
    /// poisoned. Analysis fails closed rather than silently losing its timer.
    /// On an owner failure, pending analyses may receive a forced deadline
    /// notification (BudgetExhausted or an existing incumbent); this is not a
    /// typed failure delivered to already-running calls. Registrations attempted
    /// while the owner lock is poisoned are refused; a start failure refuses that
    /// analysis and a later analysis may start a fresh worker.
    TimerUnavailable,
}

/// Distinct refusals from a planning attempt. Every variant carries the stage
/// where the attempt stopped or was refused entry and a small timing summary.
/// An absent optional stage duration means that phase was not entered, rather
/// than a zero duration. A usable plan is never carried in an error and no error
/// authorises continuing inference.
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
        reason: AdmissionDeniedReason,
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

impl PlanError {
    pub(crate) fn cancelled(stage: AnalysisStage, timing: AnalysisTiming) -> Self {
        Self::Cancelled { stage, timing }
    }

    pub(crate) fn budget(stage: AnalysisStage, timing: AnalysisTiming) -> Self {
        Self::BudgetExhausted { stage, timing }
    }

    pub(crate) fn admission(
        reason: AdmissionDeniedReason,
        stage: AnalysisStage,
        timing: AnalysisTiming,
    ) -> Self {
        Self::AdmissionDenied {
            reason,
            stage,
            timing,
        }
    }

    pub(crate) fn invalid_input(
        reason: ContractError,
        stage: AnalysisStage,
        timing: AnalysisTiming,
    ) -> Self {
        Self::InvalidInput {
            reason,
            stage,
            timing,
        }
    }

    pub(crate) fn invariant(
        reason: &'static str,
        stage: AnalysisStage,
        timing: AnalysisTiming,
    ) -> Self {
        Self::Invariant {
            reason,
            stage,
            timing,
        }
    }

    pub(crate) fn from_search(
        stage: AnalysisStage,
        timing: AnalysisTiming,
        error: SearchError,
    ) -> Self {
        match error {
            SearchError::Cancelled => Self::cancelled(stage, timing),
            SearchError::BudgetExhausted => Self::budget(stage, timing),
            SearchError::AdmissionDenied => {
                Self::admission(AdmissionDeniedReason::Allocation, stage, timing)
            }
            SearchError::Invariant(reason) => Self::invariant(reason, stage, timing),
        }
    }

    pub(crate) fn from_discovery(
        stage: AnalysisStage,
        timing: AnalysisTiming,
        error: DiscoveryError,
    ) -> Self {
        match error {
            DiscoveryError::Cancelled => Self::cancelled(stage, timing),
            DiscoveryError::BudgetExhausted => Self::budget(stage, timing),
            DiscoveryError::AdmissionDenied(reason) => Self::admission(reason, stage, timing),
            DiscoveryError::Invariant(reason) => Self::invariant(reason, stage, timing),
        }
    }
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
