/* Supported-target qualification: Darwin arm64 calibration, not a universal ABI claim. */
#include "cache_reuse_utils.h"
#include <stddef.h>
#include <stdio.h>

#define CHECK_SIZE(T, N) _Static_assert(sizeof(T) == N, #T " size")
#define CHECK_ALIGN(T, N) _Static_assert(_Alignof(T) == N, #T " alignment")
#define CHECK_OFF(T, F, N) _Static_assert(offsetof(T, F) == N, #T "." #F " offset")

CHECK_SIZE(scr_planner_range, 16); CHECK_ALIGN(scr_planner_range, 4);
CHECK_OFF(scr_planner_range, source_start, 0); CHECK_OFF(scr_planner_range, destination_start, 4);
CHECK_OFF(scr_planner_range, count, 8); CHECK_OFF(scr_planner_range, prepared_start, 12);
CHECK_SIZE(scr_planner_insertion, 12); CHECK_ALIGN(scr_planner_insertion, 4);
CHECK_OFF(scr_planner_insertion, before_target_index, 0); CHECK_OFF(scr_planner_insertion, token, 4);
CHECK_OFF(scr_planner_insertion, count, 8);
CHECK_SIZE(scr_planner_config, 88); CHECK_ALIGN(scr_planner_config, 8);
CHECK_OFF(scr_planner_config, max_ranges, 0); CHECK_OFF(scr_planner_config, minimum_match_tokens, 4);
CHECK_OFF(scr_planner_config, minimum_physical_reuse_tokens, 8);
CHECK_OFF(scr_planner_config, source_start_alignment, 12);
CHECK_OFF(scr_planner_config, prepared_target_start_alignment, 16);
CHECK_OFF(scr_planner_config, reuse_count_alignment, 20);
CHECK_OFF(scr_planner_config, minimum_prepared_reuse_start, 24);
CHECK_OFF(scr_planner_config, protected_prefix_tokens, 28);
CHECK_OFF(scr_planner_config, fresh_suffix_tokens, 32);
CHECK_OFF(scr_planner_config, max_trim_left, 36); CHECK_OFF(scr_planner_config, max_trim_right, 40);
CHECK_OFF(scr_planner_config, pad_token, 44); CHECK_OFF(scr_planner_config, loss_basis_points, 48);
CHECK_OFF(scr_planner_config, max_new_padding, 52); CHECK_OFF(scr_planner_config, max_prepared_tokens, 56);
CHECK_OFF(scr_planner_config, variant_limit, 60); CHECK_OFF(scr_planner_config, budget_ms, 64);
CHECK_OFF(scr_planner_config, allow_all_insertions, 68);
CHECK_OFF(scr_planner_config, allowed_insertions, 72);
CHECK_OFF(scr_planner_config, allowed_insertion_count, 80);
CHECK_SIZE(scr_planner_result, 352); CHECK_ALIGN(scr_planner_result, 8);
CHECK_OFF(scr_planner_result, status, 0); CHECK_OFF(scr_planner_result, covered_tokens, 4);
CHECK_OFF(scr_planner_result, inserted_padding, 8); CHECK_OFF(scr_planner_result, prepared_tokens, 12);
CHECK_OFF(scr_planner_result, best_useful, 16); CHECK_OFF(scr_planner_result, admissible_min, 20);
CHECK_OFF(scr_planner_result, range_count, 24); CHECK_OFF(scr_planner_result, insertion_count, 28);
CHECK_OFF(scr_planner_result, elapsed_ns, 32); CHECK_OFF(scr_planner_result, preparation_ns, 40);
CHECK_OFF(scr_planner_result, discovery_ns, 48); CHECK_OFF(scr_planner_result, search_ns, 56);
CHECK_OFF(scr_planner_result, discovery_entered, 64); CHECK_OFF(scr_planner_result, search_entered, 65);
CHECK_OFF(scr_planner_result, timing_known, 66);
CHECK_OFF(scr_planner_result, ranges, 68); CHECK_OFF(scr_planner_result, insertions, 228);
_Static_assert(sizeof(((scr_planner_result*)0)->ranges) / sizeof(scr_planner_range) == SCR_PLANNER_MAX_RANGES, "range array");
_Static_assert(sizeof(((scr_planner_result*)0)->insertions) / sizeof(scr_planner_insertion) == SCR_PLANNER_MAX_INSERTIONS, "insertion array");
_Static_assert(SCR_PLANNER_MAX_RANGES == 10, "range constant");
_Static_assert(SCR_PLANNER_MAX_INSERTIONS == 10, "insertion constant");
_Static_assert(SCR_PLANNER_COMPLETE == 0, "status complete");
_Static_assert(SCR_PLANNER_NO_FEASIBLE == 1, "status no feasible");
_Static_assert(SCR_PLANNER_DEADLINE_PARTIAL == 2, "status deadline partial");
_Static_assert(SCR_PLANNER_VARIANT_LIMIT == 3, "status variant limit");
_Static_assert(SCR_PLANNER_STATE_LIMIT == 4, "status state limit");
_Static_assert(SCR_PLANNER_ERR_INVALID == 10, "error invalid");
_Static_assert(SCR_PLANNER_ERR_BUDGET == 11, "error budget");
_Static_assert(SCR_PLANNER_ERR_CANCELLED == 12, "error cancelled");
_Static_assert(SCR_PLANNER_ERR_ADMISSION == 13, "error admission");
_Static_assert(SCR_PLANNER_ERR_PANIC == 14, "error panic");

int main(void) {
    printf("ffi ABI: ranges=%zu insertions=%zu\n",
        sizeof(((scr_planner_result*)0)->ranges) / sizeof(scr_planner_range),
        sizeof(((scr_planner_result*)0)->insertions) / sizeof(scr_planner_insertion));
    return 0;
}
