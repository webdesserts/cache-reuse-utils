use super::*;
use crate::placement::{AllowedInsertions, LossAllowance};
use std::cell::Cell;
use std::time::{Duration, Instant};

fn range(source_start: u32, destination_start: u32, token_count: u32) -> CandidateRange {
    CandidateRange {
        source_start,
        destination_start,
        token_count,
    }
}

/// Search with a test clock that expires after `reads` clock reads. Every
/// tick reads the clock under the test seam.
fn search_until(
    context_len: usize,
    matches: &[CandidateRange],
    k: u32,
    policy: &PlacementPolicy<'_>,
    reads: usize,
    max_states: usize,
) -> Result<PlacementOutcome, PlanErrorKind> {
    let base = Instant::now();
    let deadline = base + Duration::from_secs(5);
    let count = Cell::new(0);
    let mut cancel = || false;
    let mut stopper = Stopper::with_clock(deadline, &mut cancel, |_| {
        count.set(count.get() + 1);
        if count.get() > reads { deadline } else { base }
    });
    search_with_cap(context_len, matches, k, policy, &mut stopper, max_states)
}

fn run(
    context_len: usize,
    matches: &[CandidateRange],
    k: u32,
    policy: &PlacementPolicy<'_>,
) -> PlacementOutcome {
    search_until(context_len, matches, k, policy, usize::MAX, MAX_STATES).expect("search")
}

fn padded<'a>(context_len: usize, max_new_padding: u32) -> PlacementPolicy<'a> {
    let mut policy = PlacementPolicy::with_padding(
        context_len,
        99,
        AllowedInsertions::All,
        context_len + max_new_padding as usize,
    );
    policy.prepared_target_start_alignment = 2;
    policy
}

fn spans(outcome: &PlacementOutcome) -> Vec<(u32, u32, u32)> {
    outcome
        .plan
        .iter()
        .map(|r| (r.source_start, r.destination_start, r.token_count))
        .collect()
}

#[test]
fn fit_trims_the_overlap_then_meets_alignment() {
    let mut policy = PlacementPolicy::without_padding(100);
    let variant = Variant {
        source_start: 10,
        destination_start: 20,
        token_count: 10,
    };
    let fitted = |after, policy: &PlacementPolicy<'_>| {
        fit(variant, after, policy).map(|v| (v.source_start, v.destination_start, v.token_count))
    };
    // The larger of the destination (3) and source (1) overlaps wins.
    assert_eq!(fitted(Some((23, 11)), &policy), Some((13, 23, 7)));
    policy.source_start_alignment = 4;
    assert_eq!(fitted(Some((23, 11)), &policy), Some((16, 26, 4)));
    policy.reuse_count_alignment = 3;
    assert_eq!(fitted(None, &policy), Some((12, 22, 6)));
    assert_eq!(fitted(Some((29, 0)), &policy), None);
}

#[test]
fn overlapping_matches_chain_without_a_trim_allowance() {
    let policy = PlacementPolicy::without_padding(300);
    let matches = [range(0, 0, 100), range(101, 90, 210)];
    let outcome = run(300, &matches, 2, &policy);
    assert_eq!(spans(&outcome), [(0, 0, 100), (111, 100, 200)]);
    assert_eq!(outcome.covered, 300);
}

#[test]
fn a_trimmed_piece_and_its_continuation_coalesce_into_one_range() {
    // Right-trim variants of a match can chain back onto the same diagonal;
    // the plan must still report one range.
    let mut policy = PlacementPolicy::without_padding(20);
    policy.max_trim_right = 2;
    let outcome = run(20, &[range(0, 0, 6)], 1, &policy);
    assert_eq!(spans(&outcome), [(0, 0, 6)]);
}

#[test]
fn padding_is_inserted_at_the_latest_allowed_boundary() {
    let mut policy = padded(20, 4);
    policy.allowed_insertions = AllowedInsertions::Boundaries(&[2, 4]);
    let outcome = run(20, &[range(0, 5, 10)], 1, &policy);
    assert_eq!(outcome.padding, 1);
    assert_eq!(outcome.insertions[0].before_target_index, 4);
    assert_eq!(outcome.plan[0].prepared_start, 6);
}

#[test]
fn pad_penalty_trades_coverage_for_padding_within_the_floor() {
    let matches = [range(0, 4, 900), range(2000, 1, 980)];
    let mut policy = padded(1000, 2);
    policy.loss_allowance = LossAllowance::from_percent(10);
    // 980 needs one pad; 980 - 16 beats 900.
    assert_eq!(run(1000, &matches, 1, &policy).covered, 980);
    // At 100 tokens per pad, 900 with no padding wins.
    policy.pad_penalty_tokens = 100;
    let outcome = run(1000, &matches, 1, &policy);
    assert_eq!((outcome.covered, outcome.padding), (900, 0));
    assert_eq!(outcome.best_useful, 980);
    // With no allowance the best coverage is mandatory regardless of cost.
    policy.loss_allowance = LossAllowance::from_percent(0);
    assert_eq!(run(1000, &matches, 1, &policy).covered, 980);
}

#[test]
fn full_allowance_returns_empty_when_padding_costs_more_than_it_buys() {
    let mut policy = padded(20, 1);
    policy.loss_allowance = LossAllowance::from_percent(100);
    let outcome = run(20, &[range(0, 1, 5)], 1, &policy);
    assert!(outcome.plan.is_empty());
    assert_eq!(outcome.best_useful, 5);
    assert_eq!(outcome.status, PlacementStatus::Complete);
}

/// Segments of increasing length, so the longest are last in destination
/// order: a prefix-first search that stops early would keep the shortest.
fn segments(count: u32) -> (usize, Vec<CandidateRange>) {
    let mut matches = Vec::new();
    let mut at = 0;
    for i in 0..count {
        let len = 8 + i / 10;
        matches.push(range(at, at, len));
        at += len + 1;
    }
    (at as usize, matches)
}

#[test]
fn an_early_deadline_returns_the_greedy_incumbent() {
    let (len, matches) = segments(200);
    let policy = PlacementPolicy::without_padding(len);
    // Reads for two derivation passes and greedy, then a few search steps.
    let outcome = search_until(
        len,
        &matches,
        10,
        &policy,
        3 * matches.len() + 5,
        MAX_STATES,
    )
    .expect("incumbent");
    assert_eq!(outcome.status, PlacementStatus::DeadlinePartial);
    let longest: u64 = matches
        .iter()
        .rev()
        .take(10)
        .map(|m| m.token_count as u64)
        .sum();
    assert_eq!(outcome.covered, longest);
}

#[test]
fn a_deadline_before_any_plan_is_budget_exhausted() {
    let policy = PlacementPolicy::without_padding(20);
    assert_eq!(
        search_until(20, &[range(0, 0, 4)], 1, &policy, 0, MAX_STATES).unwrap_err(),
        PlanErrorKind::BudgetExhausted
    );
}

#[test]
fn the_state_cap_keeps_the_best_plan_so_far() {
    let (len, matches) = segments(50);
    let policy = PlacementPolicy::without_padding(len);
    let outcome = search_until(len, &matches, 3, &policy, usize::MAX, 20).expect("capped");
    assert_eq!(outcome.status, PlacementStatus::StateLimitReached);
    let longest: u64 = matches
        .iter()
        .rev()
        .take(3)
        .map(|m| m.token_count as u64)
        .sum();
    assert_eq!(outcome.covered, longest);
}

#[test]
fn cancellation_discards_the_incumbent() {
    let (len, matches) = segments(50);
    let policy = PlacementPolicy::without_padding(len);
    let base = Instant::now();
    let ticks = Cell::new(0);
    let mut cancel = || {
        ticks.set(ticks.get() + 1);
        ticks.get() > 200
    };
    let mut stopper = Stopper::with_clock(base + Duration::from_secs(5), &mut cancel, |_| base);
    assert_eq!(
        search(len, &matches, 3, &policy, &mut stopper).unwrap_err(),
        PlanErrorKind::Cancelled
    );
}

#[test]
fn the_variant_cap_keeps_untrimmed_matches_first() {
    let mut policy = PlacementPolicy::without_padding(40);
    policy.max_trim_left = 3;
    policy.max_variants = 2;
    let base = Instant::now();
    let mut cancel = || false;
    let mut stopper = Stopper::with_clock(base + Duration::from_secs(5), &mut cancel, |_| base);
    let (variants, truncated) =
        derive_variants(&[range(0, 0, 8), range(20, 20, 8)], &policy, &mut stopper)
            .expect("variants");
    assert!(truncated);
    assert_eq!(
        variants.iter().map(|v| v.token_count).collect::<Vec<_>>(),
        [8, 8]
    );
}
