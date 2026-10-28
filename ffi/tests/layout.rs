use cache_reuse_utils_ffi::{
    SCR_PLANNER_COMPLETE, SCR_PLANNER_DEADLINE_PARTIAL, SCR_PLANNER_ERR_ADMISSION,
    SCR_PLANNER_ERR_BUDGET, SCR_PLANNER_ERR_CANCELLED, SCR_PLANNER_ERR_INVALID,
    SCR_PLANNER_ERR_PANIC, SCR_PLANNER_MAX_INSERTIONS, SCR_PLANNER_MAX_RANGES,
    SCR_PLANNER_NO_FEASIBLE, SCR_PLANNER_STATE_LIMIT, SCR_PLANNER_VARIANT_LIMIT, ScrPlannerConfig,
    ScrPlannerInsertion, ScrPlannerRange, ScrPlannerResult,
};
use std::mem::{align_of, offset_of, size_of};

#[test]
fn c_abi_layout_matches_expected_darwin_arm64_shape() {
    // This fixture calibrates the supported Darwin arm64 ABI; it is not a
    // universal-size or cross-target promise.
    assert_eq!(size_of::<ScrPlannerRange>(), 16);
    assert_eq!(align_of::<ScrPlannerRange>(), 4);
    assert_eq!(offset_of!(ScrPlannerRange, source_start), 0);
    assert_eq!(offset_of!(ScrPlannerRange, destination_start), 4);
    assert_eq!(offset_of!(ScrPlannerRange, count), 8);
    assert_eq!(offset_of!(ScrPlannerRange, prepared_start), 12);

    assert_eq!(size_of::<ScrPlannerInsertion>(), 12);
    assert_eq!(align_of::<ScrPlannerInsertion>(), 4);
    assert_eq!(offset_of!(ScrPlannerInsertion, before_target_index), 0);
    assert_eq!(offset_of!(ScrPlannerInsertion, token), 4);
    assert_eq!(offset_of!(ScrPlannerInsertion, count), 8);

    assert_eq!(size_of::<ScrPlannerConfig>(), 88);
    assert_eq!(align_of::<ScrPlannerConfig>(), 8);
    assert_eq!(offset_of!(ScrPlannerConfig, max_ranges), 0);
    assert_eq!(offset_of!(ScrPlannerConfig, minimum_match_tokens), 4);
    assert_eq!(
        offset_of!(ScrPlannerConfig, minimum_physical_reuse_tokens),
        8
    );
    assert_eq!(offset_of!(ScrPlannerConfig, source_start_alignment), 12);
    assert_eq!(
        offset_of!(ScrPlannerConfig, prepared_target_start_alignment),
        16
    );
    assert_eq!(offset_of!(ScrPlannerConfig, reuse_count_alignment), 20);
    assert_eq!(
        offset_of!(ScrPlannerConfig, minimum_prepared_reuse_start),
        24
    );
    assert_eq!(offset_of!(ScrPlannerConfig, protected_prefix_tokens), 28);
    assert_eq!(offset_of!(ScrPlannerConfig, fresh_suffix_tokens), 32);
    assert_eq!(offset_of!(ScrPlannerConfig, max_trim_left), 36);
    assert_eq!(offset_of!(ScrPlannerConfig, max_trim_right), 40);
    assert_eq!(offset_of!(ScrPlannerConfig, pad_token), 44);
    assert_eq!(offset_of!(ScrPlannerConfig, loss_basis_points), 48);
    assert_eq!(offset_of!(ScrPlannerConfig, max_new_padding), 52);
    assert_eq!(offset_of!(ScrPlannerConfig, max_prepared_tokens), 56);
    assert_eq!(offset_of!(ScrPlannerConfig, variant_limit), 60);
    assert_eq!(offset_of!(ScrPlannerConfig, budget_ms), 64);
    assert_eq!(offset_of!(ScrPlannerConfig, allow_all_insertions), 68);
    assert_eq!(offset_of!(ScrPlannerConfig, allowed_insertions), 72);
    assert_eq!(offset_of!(ScrPlannerConfig, allowed_insertion_count), 80);

    assert_eq!(size_of::<ScrPlannerResult>(), 352);
    assert_eq!(align_of::<ScrPlannerResult>(), 8);
    assert_eq!(offset_of!(ScrPlannerResult, status), 0);
    assert_eq!(offset_of!(ScrPlannerResult, covered_tokens), 4);
    assert_eq!(offset_of!(ScrPlannerResult, inserted_padding), 8);
    assert_eq!(offset_of!(ScrPlannerResult, prepared_tokens), 12);
    assert_eq!(offset_of!(ScrPlannerResult, best_useful), 16);
    assert_eq!(offset_of!(ScrPlannerResult, admissible_min), 20);
    assert_eq!(offset_of!(ScrPlannerResult, range_count), 24);
    assert_eq!(offset_of!(ScrPlannerResult, insertion_count), 28);
    assert_eq!(offset_of!(ScrPlannerResult, elapsed_ns), 32);
    assert_eq!(offset_of!(ScrPlannerResult, preparation_ns), 40);
    assert_eq!(offset_of!(ScrPlannerResult, discovery_ns), 48);
    assert_eq!(offset_of!(ScrPlannerResult, search_ns), 56);
    assert_eq!(offset_of!(ScrPlannerResult, discovery_entered), 64);
    assert_eq!(offset_of!(ScrPlannerResult, search_entered), 65);
    assert_eq!(offset_of!(ScrPlannerResult, timing_known), 66);
    assert_eq!(offset_of!(ScrPlannerResult, ranges), 68);
    assert_eq!(offset_of!(ScrPlannerResult, insertions), 228);

    assert_eq!(SCR_PLANNER_MAX_RANGES, 10);
    assert_eq!(SCR_PLANNER_MAX_INSERTIONS, 10);
    assert_eq!(size_of::<[ScrPlannerRange; SCR_PLANNER_MAX_RANGES]>(), 160);
    assert_eq!(
        size_of::<[ScrPlannerInsertion; SCR_PLANNER_MAX_INSERTIONS]>(),
        120
    );
    assert_eq!(SCR_PLANNER_COMPLETE, 0);
    assert_eq!(SCR_PLANNER_NO_FEASIBLE, 1);
    assert_eq!(SCR_PLANNER_DEADLINE_PARTIAL, 2);
    assert_eq!(SCR_PLANNER_VARIANT_LIMIT, 3);
    assert_eq!(SCR_PLANNER_STATE_LIMIT, 4);
    assert_eq!(SCR_PLANNER_ERR_INVALID, 10);
    assert_eq!(SCR_PLANNER_ERR_BUDGET, 11);
    assert_eq!(SCR_PLANNER_ERR_CANCELLED, 12);
    assert_eq!(SCR_PLANNER_ERR_ADMISSION, 13);
    assert_eq!(SCR_PLANNER_ERR_PANIC, 14);
}
