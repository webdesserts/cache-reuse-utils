//! Public analysis API tests: one configured request, one call.

use scr_planner::{AnalysisStage, CacheReuseAnalysis, CandidateRange, PlanError, ReusePlan};
use std::time::{Duration, Instant};

fn range(source_start: u32, destination_start: u32, token_count: u32) -> CandidateRange {
    CandidateRange {
        source_start,
        destination_start,
        token_count,
    }
}

fn analyze(candidates: &[CandidateRange], k: u32) -> Result<ReusePlan, PlanError> {
    let mut cancel = || false;
    CacheReuseAnalysis {
        candidates,
        k,
        deadline: Instant::now() + Duration::from_secs(5),
        cancel: &mut cancel,
    }
    .analyze()
}

#[test]
fn returns_a_usable_plan_and_metadata() {
    let candidates = [
        range(20, 0, 10), // competing alternative for destination 0
        range(0, 0, 8),
        range(40, 20, 3),
    ];
    let plan = analyze(&candidates, 1).expect("selects a plan");
    assert_eq!(plan.ranges(), &[range(20, 0, 10)]);
    assert_eq!(plan.covered_tokens(), 10);
    let timing = plan.timing();
    assert!(!timing.reached());
    assert!(timing.limit() <= Duration::from_secs(5));
    assert!(timing.search().is_some());
    assert_eq!(
        timing.elapsed(),
        timing.preparation() + timing.search().unwrap()
    );
}

#[test]
fn empty_input_is_deliberate_no_reuse_not_an_error() {
    let plan = analyze(&[], 1).expect("empty no-reuse plan");
    assert!(plan.ranges().is_empty());
    assert_eq!(plan.covered_tokens(), 0);
    assert!(plan.timing().search().is_none());
}

#[test]
fn cancellation_wins_even_for_empty_input() {
    let mut cancel = || true;
    let error = CacheReuseAnalysis {
        candidates: &[],
        k: 1,
        deadline: Instant::now() + Duration::from_secs(5),
        cancel: &mut cancel,
    }
    .analyze()
    .expect_err("cancelled request must not return a plan");
    assert!(matches!(
        error,
        PlanError::Cancelled {
            stage: AnalysisStage::Preparation,
            ..
        }
    ));
}

#[test]
fn invalid_input_is_reported_without_a_plan() {
    let error = analyze(&[range(0, 0, 0)], 1).expect_err("zero-length candidate is invalid");
    assert!(matches!(
        error,
        PlanError::InvalidInput {
            stage: AnalysisStage::Preparation,
            ..
        }
    ));

    let mut cancel = || false;
    let error = CacheReuseAnalysis {
        candidates: &[range(0, 0, 1)],
        k: 1,
        deadline: Instant::now()
            .checked_sub(Duration::from_secs(1))
            .expect("instant supports subtraction"),
        cancel: &mut cancel,
    }
    .analyze()
    .expect_err("past deadline has no incumbent");
    assert!(matches!(
        error,
        PlanError::BudgetExhausted {
            stage: AnalysisStage::Preparation,
            timing,
        } if timing.reached()
            && timing.limit() == Duration::ZERO
            && timing.search().is_none()
            && timing.preparation() == timing.elapsed()
    ));
}
