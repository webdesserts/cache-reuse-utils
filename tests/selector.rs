//! Public selector behavior plus an independent exhaustive oracle for small pools.

use scr_planner::{CacheReuseAnalysis, CandidateRange, PlanError, ReusePlan};
use std::time::{Duration, Instant};

fn range(source_start: u32, destination_start: u32, token_count: u32) -> CandidateRange {
    CandidateRange {
        source_start,
        destination_start,
        token_count,
    }
}

fn select(candidates: &[CandidateRange], k: u32) -> Result<ReusePlan, PlanError> {
    let mut cancel = || false;
    CacheReuseAnalysis {
        candidates,
        k,
        deadline: Instant::now() + Duration::from_secs(5),
        cancel: &mut cancel,
    }
    .analyze()
}

fn key(candidate: &CandidateRange) -> (u32, u32, u32) {
    (
        candidate.destination_start,
        candidate.source_start,
        candidate.token_count,
    )
}

fn coalesce(mut selected: Vec<CandidateRange>) -> Vec<CandidateRange> {
    selected.sort_by_key(key);
    let mut plan: Vec<CandidateRange> = Vec::new();
    for candidate in selected {
        if let Some(last) = plan.last_mut()
            && last.source_end() == Some(candidate.source_start)
            && last.destination_end() == Some(candidate.destination_start)
        {
            last.token_count += candidate.token_count;
            continue;
        }
        plan.push(candidate);
    }
    plan
}

fn plan_less(a: &[CandidateRange], b: &[CandidateRange]) -> bool {
    for (left, right) in a.iter().zip(b) {
        if left != right {
            return key(left) < key(right);
        }
    }
    a.len() < b.len()
}

/// Enumerate every subset (not only subsets of size K), coalesce, then require
/// the final range count to fit K. This deliberately catches plans whose raw
/// node count exceeds K.
fn oracle(candidates: &[CandidateRange], k: u32) -> Option<Vec<CandidateRange>> {
    let mut best: Option<Vec<CandidateRange>> = None;
    let mut best_score = 0u64;
    for mask in 0u32..(1u32 << candidates.len()) {
        let selected: Vec<_> = (0..candidates.len())
            .filter(|index| mask & (1 << index) != 0)
            .map(|index| candidates[index])
            .collect();
        let plan = coalesce(selected);
        if plan.len() > k as usize || scr_planner::validate_plan(&plan, k).is_err() {
            continue;
        }
        let score: u64 = plan.iter().map(|range| range.token_count as u64).sum();
        let better = score > best_score
            || (score == best_score
                && match &best {
                    None => true,
                    Some(current) => {
                        plan.len() < current.len()
                            || (plan.len() == current.len() && plan_less(&plan, current))
                    }
                });
        if better {
            best = Some(plan);
            best_score = score;
        }
    }
    best
}

#[test]
fn full_search_controls_return_best_available_plans() {
    // These two fixtures are the full-search controls for the private mid-row
    // deadline regressions: 11 remains the unvisited best in the first pool,
    // while 10 is already the best (coalesced) result in the second.
    let first = [range(0, 0, 4), range(0, 0, 5), range(10, 10, 6)];
    let plan = select(&first, 2).expect("full search");
    assert_eq!(plan.covered_tokens(), 11);
    assert!(!plan.timing().reached());

    let second = [range(0, 0, 4), range(0, 0, 5), range(4, 4, 6)];
    let plan = select(&second, 2).expect("full search");
    assert_eq!(plan.covered_tokens(), 10);
    assert_eq!(plan.ranges(), &[range(0, 0, 10)]);
}

#[test]
fn competing_candidates_choose_best_coverage() {
    let candidates = [range(0, 0, 8), range(20, 0, 10), range(40, 20, 3)];
    let one = select(&candidates, 1).expect("plan");
    assert_eq!(one.ranges(), &[range(20, 0, 10)]);
    assert_eq!(one.covered_tokens(), 10);

    let two = select(&candidates, 2).expect("plan");
    assert_eq!(two.covered_tokens(), 13);
    assert_eq!(two.ranges(), &[range(20, 0, 10), range(40, 20, 3)]);
}

#[test]
fn adjacent_selected_ranges_are_coalesced() {
    let one = select(&[range(0, 0, 4), range(4, 4, 3)], 1).expect("coalesced plan");
    assert_eq!(one.ranges(), &[range(0, 0, 7)]);

    // Four raw selected nodes coalesce into two final ranges, so K=2 suffices.
    let nodes = [
        range(0, 0, 2),
        range(2, 2, 2),
        range(10, 10, 2),
        range(12, 12, 2),
        range(20, 20, 1),
    ];
    let two = select(&nodes, 2).expect("coalesced two-range plan");
    assert_eq!(two.ranges().len(), 2);
    assert_eq!(two.covered_tokens(), 8);
    assert_eq!(two.ranges(), &[range(0, 0, 4), range(10, 10, 4)]);
}

#[test]
fn result_is_independent_of_candidate_order() {
    let candidates = [range(0, 0, 8), range(20, 0, 10), range(40, 20, 3)];
    let expected = select(&candidates, 2).expect("plan").ranges().to_vec();
    let mut reversed = candidates;
    reversed.reverse();
    assert_eq!(select(&reversed, 2).expect("plan").ranges(), expected);
}

#[test]
fn matches_exhaustive_oracle_on_fixed_and_generated_pools() {
    let fixed = [
        range(0, 0, 4),
        range(4, 4, 3),
        range(10, 10, 2),
        range(12, 12, 2),
        range(20, 20, 1),
        range(30, 10, 5),
    ];
    for k in 1..=3 {
        let expected = oracle(&fixed, k).expect("oracle plan");
        let plan = select(&fixed, k).expect("selector plan");
        assert_eq!(plan.ranges(), expected.as_slice(), "fixed k={k}");
    }

    let mut state = 0x1234_5678u64;
    let mut next = || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((state >> 33) % 31) as u32
    };
    for case in 0..30 {
        let len = 3 + (case % 6) as usize;
        let candidates: Vec<_> = (0..len)
            .map(|_| range(next(), next(), 1 + next() % 4))
            .collect();
        let k = 1 + (case % 3) as u32;
        let expected = oracle(&candidates, k).expect("oracle plan");
        let plan = select(&candidates, k).expect("selector plan");
        assert_eq!(plan.ranges(), expected.as_slice(), "random case {case}");
        assert_eq!(
            plan.covered_tokens(),
            expected.iter().map(|r| r.token_count as u64).sum::<u64>()
        );
    }
}
