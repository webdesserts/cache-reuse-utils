//! Placement policy behavior through the public analysis API.

use cache_reuse_utils::{
    AllowedInsertions, AnalysisStage, CacheIdentity, CacheReuseAnalysis, CacheView, ContractError,
    DiscoveryStatus, LossAllowance, MAX_RANGES, PlacementConfigError, PlacementPolicy,
    PlacementStatus, PlanError, PlanErrorKind, ReusePlan,
};
use std::time::{Duration, Instant};

fn analyze(
    context: &[u32],
    cache: &[u32],
    minimum: u32,
    k: u32,
    policy: PlacementPolicy<'_>,
) -> Result<ReusePlan, PlanError> {
    let mut cancel = || false;
    CacheReuseAnalysis {
        context_tokens: context,
        cache: CacheView {
            tokens: cache,
            identity: CacheIdentity::new(7),
            minimum_match_tokens: minimum,
        },
        k,
        deadline: Instant::now() + Duration::from_secs(30),
        cancel: &mut cancel,
        placement: policy,
    }
    .analyze()
}

fn spans(plan: &ReusePlan) -> Vec<(u32, u32, u32)> {
    plan.ranges()
        .iter()
        .map(|r| (r.source_start, r.destination_start, r.token_count))
        .collect()
}

const CONTEXT: [u32; 10] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
const CACHE: [u32; 6] = [1, 2, 3, 4, 5, 6];

/// One spare token of capacity and even prepared starts, so reusing
/// `CACHE` at destination 1 needs one pad.
fn even_starts<'a>(allowed: AllowedInsertions<'a>) -> PlacementPolicy<'a> {
    let mut policy = PlacementPolicy::with_padding(10, 99, allowed, 11);
    policy.prepared_target_start_alignment = 2;
    policy
}

#[test]
fn padding_is_inserted_at_the_latest_allowed_boundary() {
    let plan = analyze(&CONTEXT, &CACHE, 6, 1, even_starts(AllowedInsertions::All)).unwrap();
    let recipe = plan.placement();
    assert_eq!(plan.covered_tokens(), 6);
    assert_eq!(
        (
            recipe.insertions[0].before_target_index,
            recipe.insertions[0].count
        ),
        (1, 1)
    );
    assert_eq!(
        recipe.materialize(&CONTEXT, &CACHE).unwrap(),
        [0, 99, 1, 2, 3, 4, 5, 6, 7, 8, 9]
    );
}

/// The two policies under which `CACHE` at destination 1 cannot be padded:
/// the only allowed insertion is outside the gap, or padding is disabled.
fn no_usable_insertion<'a>() -> [PlacementPolicy<'a>; 2] {
    let outside_gap = even_starts(AllowedInsertions::Boundaries(&[2]));
    let mut no_padding = PlacementPolicy::without_padding(10);
    no_padding.prepared_target_start_alignment = 2;
    [outside_gap, no_padding]
}

#[test]
fn an_unaligned_match_is_left_trimmed_when_it_cannot_be_padded() {
    for policy in no_usable_insertion() {
        let plan = analyze(&CONTEXT, &CACHE, 6, 1, policy).unwrap();
        assert_eq!(spans(&plan), [(1, 2, 5)]);
        assert_eq!(plan.ranges()[0].prepared_start, 2);
        assert!(plan.placement().insertions.is_empty());
        assert_eq!(
            plan.placement().materialize(&CONTEXT, &CACHE).unwrap(),
            CONTEXT
        );
    }
}

#[test]
fn matches_that_neither_padding_nor_trimming_can_place_are_no_feasible_reuse() {
    for mut policy in no_usable_insertion() {
        // Trimming to an aligned start would leave 5 tokens.
        policy.minimum_physical_reuse_tokens = 6;
        let plan = analyze(&CONTEXT, &CACHE, 6, 1, policy).unwrap();
        assert!(plan.ranges().is_empty());
        assert_eq!(plan.placement_status(), PlacementStatus::NoFeasibleReuse);
    }
}

#[test]
fn full_allowance_prefers_a_left_trim_to_an_expensive_pad() {
    let mut policy = even_starts(AllowedInsertions::All);
    policy.loss_allowance = LossAllowance::from_percent(100);
    let plan = analyze(&CONTEXT, &CACHE, 6, 1, policy).unwrap();
    assert_eq!(spans(&plan), [(1, 2, 5)]);
    assert_eq!(plan.metrics().best_useful, 6);
}

#[test]
fn full_allowance_prefers_no_reuse_to_an_expensive_pad() {
    let mut policy = even_starts(AllowedInsertions::All);
    policy.loss_allowance = LossAllowance::from_percent(100);
    // No trimmed alternative: it would be shorter than the minimum.
    policy.minimum_physical_reuse_tokens = 6;
    let plan = analyze(&CONTEXT, &CACHE, 6, 1, policy).unwrap();
    assert!(plan.ranges().is_empty());
    assert_eq!(plan.metrics().best_useful, 6);
}

/// `blocks` distinct blocks of `len` tokens, in the same order in both
/// sequences, separated by edits of different lengths so each block sits on
/// its own diagonal. Block `i` is followed by `strays[i]` tokens shared by
/// both edits.
fn moved_blocks(blocks: u32, len: u32, strays: &[u32]) -> (Vec<u32>, Vec<u32>) {
    let (mut context, mut cache) = (Vec::new(), Vec::new());
    for block in 0..blocks {
        let tokens = (block * 1000..block * 1000 + len).collect::<Vec<_>>();
        context.extend(&tokens);
        cache.extend(&tokens);
        let stray = strays.get(block as usize).copied().unwrap_or(0);
        let shared: Vec<u32> = (0..stray).map(|t| 500_000 + block * 100 + t).collect();
        context.push(900_000 + block);
        context.extend(&shared);
        context.push(910_000 + block);
        cache.extend([800_000 + block, 810_000 + block]);
        cache.extend(&shared);
        cache.push(820_000 + block);
    }
    (context, cache)
}

#[test]
fn up_to_32_ranges_reuse_many_moved_blocks() {
    let (context, cache) = moved_blocks(20, 40, &[]);
    let plan = analyze(
        &context,
        &cache,
        4,
        MAX_RANGES,
        PlacementPolicy::without_padding(context.len()),
    )
    .unwrap();
    assert_eq!(plan.ranges().len(), 20);
    assert_eq!(plan.covered_tokens(), 800);
    assert_eq!(plan.placement_status(), PlacementStatus::Complete);
}

#[test]
fn each_extra_range_must_cover_more_than_the_range_penalty() {
    // Three 100-token blocks; each edit after the first two shares a
    // 5-token stray phrase with the cache.
    let (context, cache) = moved_blocks(3, 100, &[5, 5]);
    let mut policy = PlacementPolicy::without_padding(context.len());
    let plan = analyze(&context, &cache, 4, 8, policy).unwrap();
    assert_eq!((plan.ranges().len(), plan.covered_tokens()), (3, 300));
    assert_eq!(plan.metrics().best_useful, 310);
    // Cheap ranges make the strays worth their slots.
    policy.range_penalty_tokens = 4;
    let plan = analyze(&context, &cache, 4, 8, policy).unwrap();
    assert_eq!((plan.ranges().len(), plan.covered_tokens()), (5, 310));
}

#[test]
fn the_first_range_is_free() {
    // Six tokens are fewer than one range penalty, but a single range is
    // never charged, even when the full allowance permits an empty plan.
    let mut policy = PlacementPolicy::without_padding(CONTEXT.len());
    policy.loss_allowance = LossAllowance::from_percent(100);
    let plan = analyze(&CONTEXT, &CACHE, 4, 1, policy).unwrap();
    assert_eq!(plan.covered_tokens(), 6);
}

#[test]
fn matches_crossing_the_protected_prefix_or_fresh_suffix_are_clipped() {
    let tokens = [1, 2, 3, 4, 5, 6];
    let mut policy = PlacementPolicy::without_padding(6);
    policy.protected_prefix_tokens = 1;
    policy.fresh_suffix_tokens = 2;
    let plan = analyze(&tokens, &tokens, 3, 1, policy).unwrap();
    assert_eq!(spans(&plan), [(1, 1, 3)]);
}

#[test]
fn overlapping_matches_combine_without_a_trim_allowance() {
    let context: Vec<u32> = (1000..1300).collect();
    let mut cache = context[..100].to_vec();
    cache.push(5);
    cache.extend_from_slice(&context[90..]);
    let plan = analyze(
        &context,
        &cache,
        4,
        2,
        PlacementPolicy::without_padding(300),
    )
    .unwrap();
    assert_eq!(spans(&plan), [(0, 0, 100), (111, 100, 200)]);
    assert_eq!(
        plan.placement().materialize(&context, &cache).unwrap(),
        context
    );
}

#[test]
fn alignment_trims_need_no_trim_allowance() {
    let context = [0, 1, 2, 3, 4, 5, 6, 7];
    let cache = [9, 1, 2, 3, 4, 5, 6, 7];
    let mut policy = PlacementPolicy::without_padding(8);
    policy.source_start_alignment = 2;
    policy.reuse_count_alignment = 4;
    let plan = analyze(&context, &cache, 7, 1, policy).unwrap();
    assert_eq!(spans(&plan), [(2, 2, 4)]);
}

#[test]
fn a_late_long_match_survives_the_candidate_cap() {
    // 70 repeated blocks give 4,900 short matches before one long shared run.
    let block: Vec<u32> = (500..516).collect();
    let (mut context, mut cache) = (Vec::new(), Vec::new());
    for i in 0..70 {
        context.extend_from_slice(&block);
        context.push(10_000 + i);
        cache.extend_from_slice(&block);
        cache.push(20_000 + i);
    }
    context.extend(100_000..102_000);
    cache.extend(100_000..102_000);
    let policy = PlacementPolicy::without_padding(context.len());
    let plan = analyze(&context, &cache, 4, 1, policy).unwrap();
    assert_eq!(
        plan.discovery_status(),
        DiscoveryStatus::CandidateLimitReached
    );
    assert_eq!(plan.covered_tokens(), 2000);
}

#[test]
fn variant_cap_returns_a_plan_or_reports_incomplete() {
    let context = [1, 2, 3, 4, 0, 5, 6, 7, 8];
    let cache = [1, 2, 3, 4, 9, 5, 6, 7, 8];
    let mut policy = PlacementPolicy::without_padding(9);
    policy.max_variants = 1;
    let plan = analyze(&context, &cache, 4, 2, policy).unwrap();
    assert_eq!(
        plan.placement_status(),
        PlacementStatus::VariantLimitReached
    );
    assert_eq!(plan.covered_tokens(), 4);

    // The only variant would need a pad to start at prepared position 1, and
    // trimming it to start there would leave fewer than 4 tokens.
    policy.minimum_prepared_reuse_start = 1;
    policy.minimum_physical_reuse_tokens = 4;
    let error = analyze(&context, &cache, 4, 2, policy).unwrap_err();
    assert_eq!(
        (error.kind, error.stage),
        (
            PlanErrorKind::PlacementIncomplete(PlacementStatus::VariantLimitReached),
            AnalysisStage::Search
        )
    );
}

#[test]
fn prepared_capacity_is_cumulative_across_gaps() {
    let context: Vec<u32> = (1000..1014).collect();
    let cache = [
        1001, 2000, 2001, 2002, 2003, 1002, 2004, 2005, 2006, 1006, 2007,
    ];
    for (capacity, covered) in [(15, 2), (16, 3)] {
        let mut policy =
            PlacementPolicy::with_padding(context.len(), 99, AllowedInsertions::All, capacity);
        policy.prepared_target_start_alignment = 2;
        policy.pad_penalty_tokens = 0;
        policy.loss_allowance = LossAllowance::from_percent(0);
        let plan = analyze(&context, &cache, 1, 3, policy).unwrap();
        assert!(plan.metrics().prepared_tokens as usize <= capacity);
        assert_eq!(plan.covered_tokens(), covered, "capacity {capacity}");
        assert_eq!(
            plan.placement()
                .materialize(&context, &cache)
                .unwrap()
                .len(),
            plan.metrics().prepared_tokens as usize
        );
    }
}

#[test]
fn max_ranges_are_supported_and_one_more_is_rejected() {
    let blocks = 100..100 + MAX_RANGES;
    let context: Vec<u32> = blocks.clone().flat_map(|t| [t, 0]).collect();
    let cache: Vec<u32> = blocks.flat_map(|t| [t, 99]).collect();
    let mut policy = PlacementPolicy::without_padding(context.len());
    // With no loss allowance the best coverage is mandatory, whatever the
    // range penalty.
    policy.loss_allowance = LossAllowance::from_percent(0);
    let plan = analyze(&context, &cache, 1, MAX_RANGES, policy).unwrap();
    assert_eq!(plan.ranges().len(), MAX_RANGES as usize);
    assert_eq!(
        analyze(&context, &cache, 1, MAX_RANGES + 1, policy)
            .unwrap_err()
            .kind,
        PlanErrorKind::InvalidInput(ContractError::InvalidK {
            k: MAX_RANGES + 1,
            max: MAX_RANGES
        })
    );
}

#[test]
fn invalid_policies_are_preparation_errors() {
    let mut allowance = PlacementPolicy::without_padding(4);
    allowance.loss_allowance = LossAllowance::from_percent(101);
    let capacity = PlacementPolicy::with_padding(4, 99, AllowedInsertions::All, 3);
    for (policy, expected) in [
        (
            allowance,
            PlacementConfigError::InvalidLossAllowance {
                basis_points: 10_100,
            },
        ),
        (
            capacity,
            PlacementConfigError::InvalidCapacity { got: 3, min: 4 },
        ),
    ] {
        let error = analyze(&[1, 2, 3, 4], &[1, 2, 3, 4], 4, 1, policy).unwrap_err();
        assert_eq!(error.kind, PlanErrorKind::InvalidPlacementPolicy(expected));
        assert_eq!(error.stage, AnalysisStage::Preparation);
    }
}
