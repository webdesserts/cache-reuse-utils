//! Public stateless placement API tests.

use scr_planner::{
    AnalysisStage, CacheIdentity, CacheReuseAnalysis, CacheView, LossAllowance,
    PlacementConfigError, PlacementPolicy, PlacementStatus, PlanError, ReusePlan,
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
        deadline: Instant::now() + Duration::from_secs(5),
        cancel: &mut cancel,
        placement: policy,
    }
    .analyze()
}

fn padded_policy<'a>() -> PlacementPolicy<'a> {
    let mut policy = PlacementPolicy::with_padding(10, 99, &[0, 1], 11);
    // The model for this fixture requires even prepared starts; this is a
    // caller constraint, not a default of the generic padding constructor.
    policy.prepared_target_start_alignment = 2;
    policy
}

#[test]
fn placement_recipe_inserts_at_canonical_latest_allowed_boundary() {
    let context = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
    let cache = [1, 2, 3, 4, 5, 6];
    let plan = analyze(&context, &cache, 6, 1, padded_policy()).expect("padded plan");
    assert_eq!(plan.covered_tokens(), 6);
    let recipe = plan.placement();
    assert_eq!(recipe.insertions.len(), 1);
    assert_eq!(recipe.insertions[0].before_target_index, 1);
    assert_eq!(recipe.insertions[0].token, 99);
    assert_eq!(recipe.insertions[0].count, 1);
    assert_eq!(recipe.prepared_tokens, 11);
    assert_eq!(plan.metrics().inserted_padding, 1);
    assert_eq!(plan.placement_status(), PlacementStatus::Complete);
    assert_eq!(
        recipe.materialize(&context, &cache).expect("materialize"),
        vec![0, 99, 1, 2, 3, 4, 5, 6, 7, 8, 9]
    );
}

#[test]
fn original_pad_valued_tokens_remain_useful_source_and_target_content() {
    let context = [1, 77, 2, 3];
    let cache = [1, 77, 2, 3];
    let policy = PlacementPolicy::without_padding(4);
    let plan = analyze(&context, &cache, 4, 1, policy).expect("space/token content");
    assert_eq!(plan.covered_tokens(), 4);
    assert_eq!(plan.metrics().inserted_padding, 0);
    assert!(plan.placement().insertions.is_empty());
    assert_eq!(
        plan.placement()
            .materialize(&context, &cache)
            .expect("materialize"),
        context
    );
}

#[test]
fn disallowed_gap_insertion_produces_no_feasible_reuse() {
    let context = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
    let cache = [1, 2, 3, 4, 5, 6];
    let mut policy = padded_policy();
    policy.allowed_insertions = &[2];
    let plan = analyze(&context, &cache, 6, 1, policy).expect("empty no-feasible plan");
    assert!(plan.ranges().is_empty());
    assert_eq!(plan.placement_status(), PlacementStatus::NoFeasibleReuse);
}

#[test]
fn protected_prefix_and_fresh_suffix_block_reuse() {
    let context = [1, 2, 3, 4, 5, 6];
    let cache = [1, 2, 3, 4, 5, 6];
    let mut policy = PlacementPolicy::without_padding(6);
    policy.protected_prefix_tokens = 1;
    let plan = analyze(&context, &cache, 6, 1, policy).expect("prefix empty");
    assert_eq!(plan.placement_status(), PlacementStatus::NoFeasibleReuse);

    let mut policy = PlacementPolicy::without_padding(6);
    policy.fresh_suffix_tokens = 1;
    let plan = analyze(&context, &cache, 6, 1, policy).expect("suffix empty");
    assert_eq!(plan.placement_status(), PlacementStatus::NoFeasibleReuse);
}

#[test]
fn hundred_percent_allowance_can_choose_empty_after_a_positive_plan() {
    let mut policy = padded_policy();
    policy.loss_allowance = LossAllowance::from_basis_points(10_000);
    let context = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
    let cache = [1, 2, 3, 4, 5, 6];
    let plan = analyze(&context, &cache, 6, 1, policy).expect("empty wins at 100%");
    assert!(plan.ranges().is_empty());
    assert_eq!(plan.metrics().best_useful, 6);
    assert_eq!(plan.placement_status(), PlacementStatus::Complete);
}

#[test]
fn invalid_placement_policy_is_preparation_error() {
    let mut policy = PlacementPolicy::without_padding(4);
    policy.loss_allowance = LossAllowance::from_basis_points(10_001);
    let error = analyze(&[1, 2, 3, 4], &[1, 2, 3, 4], 4, 1, policy).expect_err("invalid allowance");
    assert!(matches!(
        error,
        PlanError::InvalidPlacementPolicy {
            reason: PlacementConfigError::InvalidLossAllowance { .. },
            stage: AnalysisStage::Preparation,
            ..
        }
    ));
}

#[test]
fn variant_limit_returns_completed_plan_with_explicit_status() {
    let context = [1, 2, 3, 4, 0, 5, 6, 7, 8];
    let cache = [1, 2, 3, 4, 9, 5, 6, 7, 8];
    let mut policy = PlacementPolicy::without_padding(9);
    policy.max_variants = 1;
    let plan = analyze(&context, &cache, 4, 1, policy).expect("limited plan");
    assert_eq!(
        plan.placement_status(),
        PlacementStatus::VariantLimitReached
    );
    assert_eq!(plan.covered_tokens(), 4);
}

#[test]
fn declared_trim_rectangle_can_remove_alignment_incompatible_edge() {
    let context = [0, 1, 2, 3, 4, 5, 6, 7];
    let cache = [1, 2, 3, 4, 5, 6, 7];
    let mut policy = PlacementPolicy::without_padding(8);
    policy.max_trim_right = 1;
    policy.reuse_count_alignment = 2;
    let plan = analyze(&context, &cache, 7, 1, policy).expect("trimmed plan");
    assert_eq!(plan.covered_tokens(), 6);
    assert_eq!(plan.ranges()[0].destination_start, 1);
    assert_eq!(plan.ranges()[0].token_count, 6);
}

#[test]
fn source_extra_token_breaks_the_match_without_metadata_skipping() {
    let context = [1, 2, 3, 99, 4, 5, 6];
    let cache = [1, 2, 3, 0, 4, 5, 6];
    let policy = PlacementPolicy::without_padding(7);
    let plan = analyze(&context, &cache, 3, 2, policy).expect("split matches");
    assert_eq!(plan.covered_tokens(), 6);
    assert_eq!(plan.ranges().len(), 2);
    assert_eq!(
        plan.placement()
            .materialize(&context, &cache)
            .expect("materialize"),
        context
    );
}

#[test]
fn variant_limit_without_any_plan_is_an_informative_incomplete_error() {
    let context = [1, 2, 3, 4, 9, 5, 6, 7, 8];
    let cache = [1, 2, 3, 4, 0, 5, 6, 7, 8];
    let mut policy = PlacementPolicy::without_padding(9);
    policy.protected_prefix_tokens = 9;
    policy.max_variants = 1;
    let error = analyze(&context, &cache, 4, 1, policy).expect_err("no feasible variant");
    assert!(matches!(
        error,
        PlanError::PlacementIncomplete {
            status: PlacementStatus::VariantLimitReached,
            stage: AnalysisStage::Search,
            ..
        }
    ));
}

#[test]
fn capacity_is_cumulative_across_multiple_fresh_gaps() {
    let context: Vec<u32> = (1000..1014).collect();
    let cache = [
        1001, 2000, 2001, 2002, 2003, 1002, 2004, 2005, 2006, 1006, 2007,
    ];
    let allowed: Vec<u32> = (0..=14).collect();
    let mut policy = PlacementPolicy::with_padding(context.len(), 99, &allowed, 15);
    policy.prepared_target_start_alignment = 2;
    // Supply more padding budget than capacity allows so this case isolates
    // cumulative prepared-capacity enforcement rather than the derived budget.
    policy.max_new_padding = 4;
    let plan = analyze(&context, &cache, 1, 3, policy).expect("capacity plan");
    assert!(
        plan.metrics().prepared_tokens <= 15,
        "declared capacity exceeded: {}",
        plan.metrics().prepared_tokens
    );
    assert_eq!(
        plan.placement()
            .materialize(&context, &cache)
            .expect("materialize capacity plan")
            .len(),
        plan.metrics().prepared_tokens as usize
    );

    let mut policy = PlacementPolicy::with_padding(context.len(), 99, &allowed, 16);
    policy.prepared_target_start_alignment = 2;
    policy.max_new_padding = 4;
    let plan = analyze(&context, &cache, 1, 3, policy).expect("control capacity plan");
    assert_eq!(plan.metrics().prepared_tokens, 16);
    assert_eq!(plan.metrics().inserted_padding, 2);
    assert_eq!(plan.covered_tokens(), 3);
}

#[test]
fn supports_exactly_ten_selected_ranges_and_rejects_eleven() {
    let context = [
        10, 0, 11, 0, 12, 0, 13, 0, 14, 0, 15, 0, 16, 0, 17, 0, 18, 0, 19, 0,
    ];
    let cache = [
        10, 99, 11, 99, 12, 99, 13, 99, 14, 99, 15, 99, 16, 99, 17, 99, 18, 99, 19, 99,
    ];
    let mut policy = PlacementPolicy::without_padding(context.len());
    policy.loss_allowance = LossAllowance::from_basis_points(0);
    let plan = analyze(&context, &cache, 1, 10, policy).expect("ten-range plan");
    assert_eq!(plan.ranges().len(), 10);
    assert_eq!(plan.covered_tokens(), 10);
    assert_eq!(plan.placement().ranges.len(), 10);
    assert_eq!(plan.placement_status(), PlacementStatus::Complete);

    let error = analyze(
        &context,
        &cache,
        1,
        11,
        PlacementPolicy::without_padding(context.len()),
    )
    .expect_err("eleven exceeds supported maximum");
    assert!(matches!(
        error,
        PlanError::InvalidInput {
            reason: scr_planner::ContractError::InvalidK { k: 11, max: 10 },
            stage: AnalysisStage::Preparation,
            ..
        }
    ));
}

#[test]
fn without_padding_refuses_geometry_that_requires_an_insertion() {
    let context = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
    let cache = [1, 2, 3, 4, 5, 6];
    let mut policy = PlacementPolicy::without_padding(context.len());
    policy.prepared_target_start_alignment = 2;
    let plan = analyze(&context, &cache, 6, 1, policy).expect("no-padding refusal");
    assert!(plan.ranges().is_empty());
    assert_eq!(plan.placement_status(), PlacementStatus::NoFeasibleReuse);
    assert_eq!(plan.metrics().inserted_padding, 0);
}

#[test]
fn loss_allowance_has_common_percent_and_exact_basis_point_forms() {
    assert_eq!(
        LossAllowance::from_percent(10),
        LossAllowance::from_basis_points(1_000)
    );
    assert_eq!(LossAllowance::default(), LossAllowance::from_percent(10));
    assert_eq!(LossAllowance::from_percent(0).basis_points(), 0);
    assert_eq!(LossAllowance::from_percent(0).percent(), 0.0);
    assert_eq!(LossAllowance::from_percent(100).basis_points(), 10_000);
    assert_eq!(LossAllowance::from_percent(100).percent(), 100.0);
    assert_eq!(LossAllowance::from_basis_points(1).percent(), 0.01);
    assert_eq!(LossAllowance::from_basis_points(1).basis_points(), 1);
}

#[test]
fn invalid_loss_allowance_inputs_fail_through_policy_validation() {
    for allowance in [
        LossAllowance::from_percent(101),
        LossAllowance::from_basis_points(10_001),
    ] {
        let mut policy = PlacementPolicy::without_padding(4);
        policy.loss_allowance = allowance;
        let error = analyze(&[1, 2, 3, 4], &[1, 2, 3, 4], 4, 1, policy)
            .expect_err("out-of-range allowance");
        assert!(matches!(
            error,
            PlanError::InvalidPlacementPolicy {
                reason: PlacementConfigError::InvalidLossAllowance { .. },
                ..
            }
        ));
    }
}

#[test]
fn padding_policy_constructor_requires_explicit_token_boundaries_and_capacity() {
    let policy = PlacementPolicy::with_padding(10, 99, &[0, 1], 11);
    assert_eq!(policy.pad_token, Some(99));
    assert_eq!(policy.allowed_insertions, &[0, 1]);
    assert_eq!(policy.max_new_padding, 1);
    assert_eq!(policy.max_prepared_tokens, 11);
    assert_eq!(policy.prepared_target_start_alignment, 1);

    let no_padding = PlacementPolicy::without_padding(10);
    assert_eq!(no_padding.pad_token, None);
    assert_eq!(no_padding.allowed_insertions, &[]);
    assert_eq!(no_padding.max_new_padding, 0);
    assert_eq!(no_padding.max_prepared_tokens, 10);
    assert_eq!(no_padding.prepared_target_start_alignment, 1);

    let invalid = PlacementPolicy::with_padding(4, 99, &[1], 3);
    let error =
        analyze(&[1, 2, 3, 4], &[1, 2, 3, 4], 4, 1, invalid).expect_err("capacity below target");
    assert!(matches!(
        error,
        PlanError::InvalidPlacementPolicy {
            reason: PlacementConfigError::InvalidCapacity { .. },
            ..
        }
    ));
}
