#ifndef CACHE_REUSE_UTILS_H
#define CACHE_REUSE_UTILS_H

#include <stdarg.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>

#define SCR_PLANNER_COMPLETE 0

#define SCR_PLANNER_NO_FEASIBLE 1

#define SCR_PLANNER_DEADLINE_PARTIAL 2

#define SCR_PLANNER_VARIANT_LIMIT 3

#define SCR_PLANNER_STATE_LIMIT 4

#define SCR_PLANNER_ERR_INVALID 10

#define SCR_PLANNER_ERR_BUDGET 11

#define SCR_PLANNER_ERR_CANCELLED 12

#define SCR_PLANNER_ERR_ADMISSION 13

#define SCR_PLANNER_ERR_PANIC 14

#define SCR_PLANNER_MAX_RANGES 10

#define SCR_PLANNER_MAX_INSERTIONS 10

typedef struct {
  uint32_t max_ranges;
  uint32_t minimum_match_tokens;
  uint32_t minimum_physical_reuse_tokens;
  uint32_t source_start_alignment;
  uint32_t prepared_target_start_alignment;
  uint32_t reuse_count_alignment;
  uint32_t minimum_prepared_reuse_start;
  uint32_t protected_prefix_tokens;
  uint32_t fresh_suffix_tokens;
  uint32_t max_trim_left;
  uint32_t max_trim_right;
  int32_t pad_token;
  uint32_t loss_basis_points;
  uint32_t max_new_padding;
  uint32_t max_prepared_tokens;
  uint32_t variant_limit;
  uint32_t budget_ms;
  bool allow_all_insertions;
  const uint32_t *allowed_insertions;
  uint32_t allowed_insertion_count;
} scr_planner_config;

typedef struct {
  uint32_t source_start;
  uint32_t destination_start;
  uint32_t count;
  uint32_t prepared_start;
} scr_planner_range;

typedef struct {
  uint32_t before_target_index;
  uint32_t token;
  uint32_t count;
} scr_planner_insertion;

typedef struct {
  uint32_t status;
  uint32_t covered_tokens;
  uint32_t inserted_padding;
  uint32_t prepared_tokens;
  uint32_t best_useful;
  uint32_t admissible_min;
  uint32_t range_count;
  uint32_t insertion_count;
  uint64_t elapsed_ns;
  uint64_t preparation_ns;
  uint64_t discovery_ns;
  uint64_t search_ns;
  bool discovery_entered;
  bool search_entered;
  bool timing_known;
  scr_planner_range ranges[SCR_PLANNER_MAX_RANGES];
  scr_planner_insertion insertions[SCR_PLANNER_MAX_INSERTIONS];
} scr_planner_result;

#ifdef __cplusplus
extern "C" {
#endif // __cplusplus

int32_t scr_planner_plan(const int32_t *target,
                         uint32_t target_len,
                         const int32_t *source,
                         uint32_t source_len,
                         uint64_t cache_identity,
                         const scr_planner_config *config,
                         scr_planner_result *result,
                         char *error,
                         size_t error_len,
                         bool (*cancel)(void*),
                         void *cancel_ud);

#ifdef __cplusplus
}  // extern "C"
#endif  // __cplusplus

#endif  /* CACHE_REUSE_UTILS_H */
