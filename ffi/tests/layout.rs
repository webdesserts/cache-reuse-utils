use cache_reuse_utils_ffi::{
    CACHE_REUSE_COMPLETE, CACHE_REUSE_DEADLINE_PARTIAL, CACHE_REUSE_ERR_ADMISSION,
    CACHE_REUSE_ERR_BUDGET, CACHE_REUSE_ERR_CANCELLED, CACHE_REUSE_ERR_INVALID,
    CACHE_REUSE_ERR_PANIC, CACHE_REUSE_MAX_INSERTIONS, CACHE_REUSE_MAX_RANGES,
    CACHE_REUSE_NO_FEASIBLE, CACHE_REUSE_STATE_LIMIT, CACHE_REUSE_VARIANT_LIMIT, CacheReuseConfig,
    CacheReuseInsertion, CacheReuseRange, CacheReuseResult,
};
use std::mem::{align_of, offset_of, size_of};

#[test]
fn c_abi_layout_matches_expected_darwin_arm64_shape() {
    // This fixture calibrates the supported Darwin arm64 ABI; it is not a
    // universal-size or cross-target promise.
    assert_eq!(size_of::<CacheReuseRange>(), 16);
    assert_eq!(align_of::<CacheReuseRange>(), 4);
    assert_eq!(offset_of!(CacheReuseRange, source_start), 0);
    assert_eq!(offset_of!(CacheReuseRange, destination_start), 4);
    assert_eq!(offset_of!(CacheReuseRange, count), 8);
    assert_eq!(offset_of!(CacheReuseRange, prepared_start), 12);

    assert_eq!(size_of::<CacheReuseInsertion>(), 12);
    assert_eq!(align_of::<CacheReuseInsertion>(), 4);
    assert_eq!(offset_of!(CacheReuseInsertion, before_target_index), 0);
    assert_eq!(offset_of!(CacheReuseInsertion, token), 4);
    assert_eq!(offset_of!(CacheReuseInsertion, count), 8);

    assert_eq!(size_of::<CacheReuseConfig>(), 88);
    assert_eq!(align_of::<CacheReuseConfig>(), 8);
    assert_eq!(offset_of!(CacheReuseConfig, max_ranges), 0);
    assert_eq!(offset_of!(CacheReuseConfig, minimum_match_tokens), 4);
    assert_eq!(
        offset_of!(CacheReuseConfig, minimum_physical_reuse_tokens),
        8
    );
    assert_eq!(offset_of!(CacheReuseConfig, source_start_alignment), 12);
    assert_eq!(
        offset_of!(CacheReuseConfig, prepared_target_start_alignment),
        16
    );
    assert_eq!(offset_of!(CacheReuseConfig, reuse_count_alignment), 20);
    assert_eq!(
        offset_of!(CacheReuseConfig, minimum_prepared_reuse_start),
        24
    );
    assert_eq!(offset_of!(CacheReuseConfig, protected_prefix_tokens), 28);
    assert_eq!(offset_of!(CacheReuseConfig, fresh_suffix_tokens), 32);
    assert_eq!(offset_of!(CacheReuseConfig, max_trim_left), 36);
    assert_eq!(offset_of!(CacheReuseConfig, max_trim_right), 40);
    assert_eq!(offset_of!(CacheReuseConfig, pad_token), 44);
    assert_eq!(offset_of!(CacheReuseConfig, loss_basis_points), 48);
    assert_eq!(offset_of!(CacheReuseConfig, max_new_padding), 52);
    assert_eq!(offset_of!(CacheReuseConfig, max_prepared_tokens), 56);
    assert_eq!(offset_of!(CacheReuseConfig, variant_limit), 60);
    assert_eq!(offset_of!(CacheReuseConfig, budget_ms), 64);
    assert_eq!(offset_of!(CacheReuseConfig, allow_all_insertions), 68);
    assert_eq!(offset_of!(CacheReuseConfig, allowed_insertions), 72);
    assert_eq!(offset_of!(CacheReuseConfig, allowed_insertion_count), 80);

    assert_eq!(size_of::<CacheReuseResult>(), 352);
    assert_eq!(align_of::<CacheReuseResult>(), 8);
    assert_eq!(offset_of!(CacheReuseResult, status), 0);
    assert_eq!(offset_of!(CacheReuseResult, covered_tokens), 4);
    assert_eq!(offset_of!(CacheReuseResult, inserted_padding), 8);
    assert_eq!(offset_of!(CacheReuseResult, prepared_tokens), 12);
    assert_eq!(offset_of!(CacheReuseResult, best_useful), 16);
    assert_eq!(offset_of!(CacheReuseResult, admissible_min), 20);
    assert_eq!(offset_of!(CacheReuseResult, range_count), 24);
    assert_eq!(offset_of!(CacheReuseResult, insertion_count), 28);
    assert_eq!(offset_of!(CacheReuseResult, elapsed_ns), 32);
    assert_eq!(offset_of!(CacheReuseResult, preparation_ns), 40);
    assert_eq!(offset_of!(CacheReuseResult, discovery_ns), 48);
    assert_eq!(offset_of!(CacheReuseResult, search_ns), 56);
    assert_eq!(offset_of!(CacheReuseResult, discovery_entered), 64);
    assert_eq!(offset_of!(CacheReuseResult, search_entered), 65);
    assert_eq!(offset_of!(CacheReuseResult, timing_known), 66);
    assert_eq!(offset_of!(CacheReuseResult, ranges), 68);
    assert_eq!(offset_of!(CacheReuseResult, insertions), 228);

    assert_eq!(CACHE_REUSE_MAX_RANGES, 10);
    assert_eq!(CACHE_REUSE_MAX_INSERTIONS, 10);
    assert_eq!(size_of::<[CacheReuseRange; CACHE_REUSE_MAX_RANGES]>(), 160);
    assert_eq!(
        size_of::<[CacheReuseInsertion; CACHE_REUSE_MAX_INSERTIONS]>(),
        120
    );
    assert_eq!(CACHE_REUSE_COMPLETE, 0);
    assert_eq!(CACHE_REUSE_NO_FEASIBLE, 1);
    assert_eq!(CACHE_REUSE_DEADLINE_PARTIAL, 2);
    assert_eq!(CACHE_REUSE_VARIANT_LIMIT, 3);
    assert_eq!(CACHE_REUSE_STATE_LIMIT, 4);
    assert_eq!(CACHE_REUSE_ERR_INVALID, 10);
    assert_eq!(CACHE_REUSE_ERR_BUDGET, 11);
    assert_eq!(CACHE_REUSE_ERR_CANCELLED, 12);
    assert_eq!(CACHE_REUSE_ERR_ADMISSION, 13);
    assert_eq!(CACHE_REUSE_ERR_PANIC, 14);
}
