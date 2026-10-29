//! Analysis errors.

use std::fmt;

use crate::placement::{PlacementConfigError, PlacementStatus};
use crate::timing::{AnalysisStage, AnalysisTiming, Stop};

/// Why the analysis request itself was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractError {
    /// `k` must be in `1..=MAX_RANGES`.
    InvalidK { k: u32, max: u32 },
    /// A cache view's minimum match length must be greater than zero.
    InvalidMinimum { minimum: u32 },
}

/// Which input exceeded the supported discovery size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputSide {
    Context,
    Cache,
}

/// Why the planner refused to take on the work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionDeniedReason {
    /// An input is longer than the supported discovery size.
    InputTooLarge {
        side: InputSide,
        got: usize,
        max: usize,
    },
    /// The allocator refused a discovery index buffer.
    Allocation,
}

/// What went wrong in a planning attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanErrorKind {
    /// The request failed validation.
    InvalidInput(ContractError),
    /// The placement policy was inconsistent or outside declared limits.
    InvalidPlacementPolicy(PlacementConfigError),
    /// The deadline passed before any valid plan was found.
    BudgetExhausted,
    /// The caller cancelled the request. Any incumbent is discarded.
    Cancelled,
    /// The planner refused the work for resource reasons.
    AdmissionDenied(AdmissionDeniedReason),
    /// A variant or state cap stopped search before any valid plan was found.
    /// The status names the cap.
    PlacementIncomplete(PlacementStatus),
    /// The planner produced an internally inconsistent plan. This is a bug.
    Invariant(&'static str),
}

/// A refused or failed planning attempt.
///
/// `stage` is where the attempt stopped or was refused entry, and `timing`
/// summarises the stages that ran. An error never carries a usable plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanError {
    pub kind: PlanErrorKind,
    pub stage: AnalysisStage,
    pub timing: AnalysisTiming,
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let stage = self.stage;
        match &self.kind {
            PlanErrorKind::InvalidInput(reason) => write!(f, "invalid input: {reason:?}"),
            PlanErrorKind::InvalidPlacementPolicy(reason) => {
                write!(f, "invalid placement policy: {reason:?}")
            }
            PlanErrorKind::BudgetExhausted => {
                write!(f, "deadline reached at {stage:?} before any plan was found")
            }
            PlanErrorKind::Cancelled => write!(f, "cancelled at {stage:?}"),
            PlanErrorKind::AdmissionDenied(reason) => write!(f, "admission denied: {reason:?}"),
            PlanErrorKind::PlacementIncomplete(status) => {
                write!(f, "search stopped at {status:?} before any plan was found")
            }
            PlanErrorKind::Invariant(reason) => write!(f, "planner invariant failed: {reason}"),
        }
    }
}

impl std::error::Error for PlanError {}

impl From<Stop> for PlanErrorKind {
    fn from(stop: Stop) -> Self {
        match stop {
            Stop::Cancelled => Self::Cancelled,
            Stop::Deadline => Self::BudgetExhausted,
        }
    }
}
