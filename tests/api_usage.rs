//! Public full-analysis API tests: context + borrowed cache -> owned plan.

use scr_planner::{
    AdmissionDeniedReason, AnalysisStage, CacheIdentity, CacheReuseAnalysis, CacheView,
    ContractError, DiscoveryStatus, InputSide, PlanError, ReusePlan,
};
use std::time::{Duration, Instant};

fn run(
    context: &[u32],
    cache_tokens: &[u32],
    minimum: u32,
    k: u32,
) -> Result<ReusePlan, PlanError> {
    let mut cancel = || false;
    let cache = CacheView {
        tokens: cache_tokens,
        identity: CacheIdentity::new(0x1234_5678),
        minimum_match_tokens: minimum,
    };
    CacheReuseAnalysis {
        context_tokens: context,
        cache,
        k,
        deadline: Instant::now() + Duration::from_secs(5),
        cancel: &mut cancel,
    }
    .analyze()
}

fn range(
    source_start: u32,
    destination_start: u32,
    token_count: u32,
) -> scr_planner::CandidateRange {
    scr_planner::CandidateRange {
        source_start,
        destination_start,
        token_count,
    }
}

#[test]
fn exact_match_is_discovered_selected_and_round_trips_identity() {
    let context = [1, 2, 3, 4, 5, 6];
    let cache = [9, 1, 2, 3, 4, 5, 6, 9];
    let plan = run(&context, &cache, 3, 1).expect("exact match plan");
    assert_eq!(plan.ranges(), &[range(1, 0, 6)]);
    assert_eq!(plan.covered_tokens(), 6);
    assert_eq!(plan.cache_identity(), CacheIdentity::new(0x1234_5678));
    assert_eq!(plan.discovery_status(), DiscoveryStatus::Complete);
    assert!(plan.timing().discovery().is_some());
    assert!(plan.timing().search().is_some());
    assert_eq!(
        plan.timing().elapsed(),
        plan.timing().preparation()
            + plan.timing().discovery().unwrap()
            + plan.timing().search().unwrap()
    );
}

#[test]
fn matches_are_found_at_all_logical_positions_not_physical_parity_strides() {
    let plan = run(&[8, 9, 1, 2, 3], &[9, 1, 2, 3], 3, 1).expect("odd-position match");
    assert_eq!(plan.ranges(), &[range(0, 1, 4)]);
}

#[test]
fn repeated_competing_matches_remain_available_to_the_selector() {
    let context = [1, 2, 3, 0, 1, 2, 3];
    let cache = [7, 1, 2, 3, 8, 1, 2, 3, 9];
    let plan = run(&context, &cache, 3, 2).expect("two matches");
    assert_eq![plan.ranges(), &[range(1, 0, 3), range(5, 4, 3)]];
    assert_eq!(plan.covered_tokens(), 6);
}

#[test]
fn minimum_filters_short_runs_and_handles_no_matches() {
    // A run shorter than the minimum is filtered after actual discovery.
    let filtered = run(&[1, 2, 3, 0], &[9, 1, 2, 3, 9], 4, 1).expect("filtered short run");
    assert!(filtered.ranges().is_empty());
    assert!(filtered.timing().discovery().is_some());
    // minimum longer than an input is a complete no-work no-match, not an error.
    let too_long = run(&[1, 2], &[9, 1, 2, 9], 4, 1).expect("minimum longer than input");
    assert!(too_long.ranges().is_empty());
    assert_eq!(too_long.discovery_status(), DiscoveryStatus::Complete);
    assert!(too_long.timing().discovery().is_none());
    assert!(too_long.timing().search().is_none());

    let actual_none = run(&[1, 2, 3], &[4, 5, 6], 2, 1).expect("complete no match");
    assert!(actual_none.ranges().is_empty());
    assert_eq!(actual_none.discovery_status(), DiscoveryStatus::Complete);
    assert!(actual_none.timing().discovery().is_some());
    assert!(actual_none.timing().search().is_none());
    assert_eq!(
        actual_none.timing().elapsed(),
        actual_none.timing().preparation() + actual_none.timing().discovery().unwrap()
    );
}

#[test]
fn empty_inputs_are_deliberate_no_reuse_with_identity() {
    let plan = run(&[], &[1, 2, 3], 1, 1).expect("empty context");
    assert!(plan.ranges().is_empty());
    assert_eq!(plan.cache_identity(), CacheIdentity::new(0x1234_5678));
    assert_eq!(plan.discovery_status(), DiscoveryStatus::Complete);
    assert!(plan.timing().discovery().is_none());
    assert!(plan.timing().search().is_none());

    let mut cancel = || false;
    let past = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .expect("instant subtraction");
    let plan = CacheReuseAnalysis {
        context_tokens: &[],
        cache: CacheView {
            tokens: &[1, 2, 3],
            identity: CacheIdentity::new(9),
            minimum_match_tokens: 1,
        },
        k: 1,
        deadline: past,
        cancel: &mut cancel,
    }
    .analyze()
    .expect("no-work no-match is complete even past deadline");
    assert!(plan.ranges().is_empty());
    assert_eq!(plan.cache_identity(), CacheIdentity::new(9));
}

#[test]
fn invalid_minimum_and_k_are_preparation_errors() {
    let min_zero = run(&[1], &[1], 0, 1).expect_err("zero minimum");
    assert!(matches!(
        min_zero,
        PlanError::InvalidInput {
            reason: ContractError::InvalidMinimum { minimum: 0 },
            stage: AnalysisStage::Preparation,
            timing,
        } if timing.discovery().is_none()
            && timing.search().is_none()
            && timing.preparation() == timing.elapsed()
    ));

    let bad_k = run(&[1], &[1], 1, 0).expect_err("zero K");
    assert!(matches!(
        bad_k,
        PlanError::InvalidInput {
            reason: ContractError::InvalidK { k: 0, .. },
            stage: AnalysisStage::Preparation,
            timing,
        } if timing.discovery().is_none()
            && timing.search().is_none()
            && timing.preparation() == timing.elapsed()
    ));
}

#[test]
fn cancellation_wins_before_discovery() {
    let mut cancel = || true;
    let error = CacheReuseAnalysis {
        context_tokens: &[1, 2, 3],
        cache: CacheView {
            tokens: &[1, 2, 3],
            identity: CacheIdentity::new(1),
            minimum_match_tokens: 1,
        },
        k: 1,
        deadline: Instant::now() + Duration::from_secs(5),
        cancel: &mut cancel,
    }
    .analyze()
    .expect_err("cancelled");
    assert!(matches!(
        error,
        PlanError::Cancelled {
            stage: AnalysisStage::Preparation,
            ..
        }
    ));
}

#[test]
fn candidate_capacity_retains_verified_matches_and_reports_partial_discovery() {
    let mut context = Vec::new();
    let mut cache = Vec::new();
    for value in 1..=4097u32 {
        context.push(value);
        context.push(200_000);
        cache.push(value);
        cache.push(100_000);
    }
    let plan = run(&context, &cache, 1, 1).expect("partial discovery plan");
    assert_eq!(
        plan.discovery_status(),
        DiscoveryStatus::CandidateLimitReached
    );
    assert_eq!(plan.covered_tokens(), 1);
}

#[test]
fn inputs_beyond_the_supported_envelope_are_admission_errors() {
    let oversized = vec![1u32; 1_048_577];
    let mut cancel = || false;
    let error = CacheReuseAnalysis {
        context_tokens: &oversized,
        cache: CacheView {
            tokens: &[1],
            identity: CacheIdentity::new(1),
            minimum_match_tokens: 1,
        },
        k: 1,
        deadline: Instant::now() + Duration::from_secs(5),
        cancel: &mut cancel,
    }
    .analyze()
    .expect_err("oversized context");
    assert!(matches!(
        error,
        PlanError::AdmissionDenied {
            reason: AdmissionDeniedReason::InputTooLarge {
                side: InputSide::Context,
                got: 1_048_577,
                max: 1_048_576,
            },
            stage: AnalysisStage::Preparation,
            ..
        }
    ));
}

fn adversarial_tokens(n: usize) -> Vec<u32> {
    let mut order: Vec<usize> = (0..n).collect();
    let mut out = vec![0u32; n];
    let mut rank = n as u64;
    while order.len() > 24 {
        let mid = order.len() / 2;
        let pos = order.remove(mid);
        out[pos] = rank as u32;
        rank -= 1;
    }
    while let Some(pos) = order.pop() {
        out[pos] = rank as u32;
        rank -= 1;
    }
    out
}

#[test]
fn crafted_seed_order_does_not_exhaust_a_small_stack() {
    let cache_tokens = adversarial_tokens(2_000);
    let result = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(move || {
            let mut cancel = || false;
            CacheReuseAnalysis {
                context_tokens: &[7],
                cache: CacheView {
                    tokens: &cache_tokens,
                    identity: CacheIdentity::new(7),
                    minimum_match_tokens: 1,
                },
                k: 1,
                deadline: Instant::now() + Duration::from_secs(10),
                cancel: &mut cancel,
            }
            .analyze()
        })
        .expect("spawn")
        .join()
        .expect("thread");
    let plan = result.expect("crafted order should return a plan");
    assert_eq!(plan.covered_tokens(), 1);
}

#[test]
fn hash_collision_is_rejected_by_exact_token_verification() {
    let plan = run(&[2, 1], &[1, 258], 2, 1).expect("collision plan");
    assert!(
        plan.ranges().is_empty(),
        "collision leaked: {:?}",
        plan.ranges()
    );
    assert_eq!(plan.discovery_status(), DiscoveryStatus::Complete);

    let control = run(&[1, 258], &[1, 258], 2, 1).expect("equal control");
    assert_eq!(control.ranges(), &[range(0, 0, 2)]);
}

#[test]
fn expiry_during_preparation_reports_budget_without_discovery_or_search() {
    let mut cancel = || false;
    let error = CacheReuseAnalysis {
        context_tokens: &[1, 2, 3],
        cache: CacheView {
            tokens: &[1, 2, 3],
            identity: CacheIdentity::new(1),
            minimum_match_tokens: 1,
        },
        k: 1,
        deadline: Instant::now()
            .checked_sub(Duration::from_secs(1))
            .expect("past instant"),
        cancel: &mut cancel,
    }
    .analyze()
    .expect_err("preparation deadline");
    assert!(matches!(
        error,
        PlanError::BudgetExhausted {
            stage: AnalysisStage::Preparation,
            timing,
        } if timing.discovery().is_none()
            && timing.search().is_none()
            && timing.preparation() == timing.elapsed()
    ));
}
