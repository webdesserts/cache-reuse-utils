#include "cache_reuse_utils.h"
#include <stdio.h>
#include <string.h>

static cache_reuse_config config_for(uint32_t k) {
    cache_reuse_config c;
    memset(&c, 0, sizeof(c));
    c.max_ranges = k;
    c.minimum_match_tokens = 1;
    c.minimum_physical_reuse_tokens = 1;
    c.source_start_alignment = 1;
    c.prepared_target_start_alignment = 1;
    c.reuse_count_alignment = 1;
    c.minimum_prepared_reuse_start = 0;
    c.protected_prefix_tokens = 0;
    c.fresh_suffix_tokens = 0;
    c.max_trim_left = 0;
    c.max_trim_right = 0;
    c.pad_token = -1;
    c.loss_basis_points = 1000;
    c.max_new_padding = 0;
    c.max_prepared_tokens = 20;
    c.variant_limit = 4096;
    c.budget_ms = 5000;
    c.allow_all_insertions = false;
    c.allowed_insertions = NULL;
    c.allowed_insertion_count = 0;
    c.pad_penalty_tokens = 0; /* default 16 when padding is enabled */
    return c;
}

static int plan(const int32_t *target, uint32_t tn, const int32_t *source, uint32_t sn,
                cache_reuse_config *cfg, cache_reuse_result *r, bool (*cancel)(void *)) {
    char err[256] = {0};
    int rc = cache_reuse_plan(target, tn, source, sn, 7, cfg, r, err, sizeof(err), cancel, NULL);
    if (rc && err[0]) fprintf(stderr, "planner error: %s\n", err);
    return rc;
}

static bool always_cancel(void *ud) { (void)ud; return true; }

int main(void) {
    int32_t target[20] = {10,0,11,0,12,0,13,0,14,0,15,0,16,0,17,0,18,0,19,0};
    int32_t source[20] = {10,99,11,99,12,99,13,99,14,99,15,99,16,99,17,99,18,99,19,99};
    cache_reuse_config cfg = config_for(10);
    cache_reuse_result r;
    memset(&r, 0, sizeof(r));
    if (plan(target, 20, source, 20, &cfg, &r, NULL) != 0 ||
        r.status != CACHE_REUSE_COMPLETE || r.range_count != 10 || r.covered_tokens != 10) {
        fprintf(stderr, "ten-range plan failed status=%u ranges=%u covered=%u\n",
                r.status, r.range_count, r.covered_tokens);
        return 1;
    }
    cfg = config_for(10);
    cfg.variant_limit = 1;
    memset(&r, 0, sizeof(r));
    if (plan(target, 20, source, 20, &cfg, &r, NULL) != 0 ||
        r.status != CACHE_REUSE_VARIANT_LIMIT || r.range_count == 0) {
        fprintf(stderr, "limited valid incumbent was not preserved: status=%u ranges=%u\n",
                r.status, r.range_count);
        return 1;
    }
    cfg = config_for(11);
    memset(&r, 0, sizeof(r));
    if (plan(target, 20, source, 20, &cfg, &r, NULL) == 0 || r.status != CACHE_REUSE_ERR_INVALID) {
        fprintf(stderr, "K11 was not rejected\n");
        return 1;
    }
    /* Historical source pad token IDs are ordinary content at this boundary:
     * the core receives actual tokens only and can still match an equal target. */
    int32_t padded_target[7] = {1, 77, 2, 3, 88, 4, 5};
    int32_t padded_source[7] = {1, 77, 2, 3, 88, 4, 5};
    cfg = config_for(10);
    cfg.max_prepared_tokens = 7;
    memset(&r, 0, sizeof(r));
    if (plan(padded_target, 7, padded_source, 7, &cfg, &r, NULL) != 0 ||
        r.range_count != 1 || r.covered_tokens != 7) {
        fprintf(stderr, "historical source token content test failed\n");
        return 1;
    }
    int32_t insert_target[6] = {1, 2, 3, 4, 5, 9};
    int32_t insert_source[4] = {2, 3, 4, 5};
    uint32_t allowed_boundary[1] = {0};
    cfg = config_for(1);
    cfg.minimum_match_tokens = 4;
    cfg.max_prepared_tokens = 7;
    cfg.max_new_padding = 1;
    cfg.pad_token = 77;
    cfg.prepared_target_start_alignment = 2;
    cfg.allowed_insertions = allowed_boundary;
    cfg.allowed_insertion_count = 1;
    memset(&r, 0, sizeof(r));
    if (plan(insert_target, 6, insert_source, 4, &cfg, &r, NULL) != 0 ||
        r.range_count != 1 || r.insertion_count != 1 ||
        r.insertions[0].token != 77 || r.insertions[0].count != 1 ||
        r.ranges[0].prepared_start != 2) {
        fprintf(stderr, "authorized insertion recipe test failed\n");
        return 1;
    }
    cfg = config_for(1);
    cfg.minimum_match_tokens = 4;
    cfg.max_prepared_tokens = 7;
    cfg.max_new_padding = 1;
    cfg.pad_token = 77;
    cfg.prepared_target_start_alignment = 2;
    cfg.allowed_insertions = NULL;
    cfg.allowed_insertion_count = 0;
    memset(&r, 0, sizeof(r));
    /* No insertion is allowed, so the match is left-trimmed to an aligned
       prepared start instead of padded. */
    if (plan(insert_target, 6, insert_source, 4, &cfg, &r, NULL) != 0 ||
        r.status != CACHE_REUSE_COMPLETE || r.range_count != 1 ||
        r.insertion_count != 0 || r.ranges[0].count != 3 ||
        r.ranges[0].prepared_start != 2) {
        fprintf(stderr, "disallowed insertion was not trimmed: status=%u ranges=%u insertions=%u\n",
                r.status, r.range_count, r.insertion_count);
        return 1;
    }
    cfg = config_for(10);
    memset(&r, 0, sizeof(r));
    if (plan(NULL, 20, source, 20, &cfg, &r, NULL) == 0 || r.status != CACHE_REUSE_ERR_INVALID) {
        fprintf(stderr, "null target was not rejected\n");
        return 1;
    }
    if (r.timing_known || r.elapsed_ns != 0) {
        fprintf(stderr, "unmeasured invalid-input timing was reported as known\n");
        return 1;
    }
    int32_t negative[20] = {10,0,11,0,12,0,13,0,14,0,15,0,16,0,17,0,18,0,19,0};
    negative[3] = -1;
    memset(&r, 0, sizeof(r));
    if (plan(target, 20, negative, 20, &cfg, &r, NULL) == 0 || r.status != CACHE_REUSE_ERR_INVALID) {
        fprintf(stderr, "negative token was not rejected\n");
        return 1;
    }
    cfg = config_for(10);
    cfg.budget_ms = 0;
    memset(&r, 0, sizeof(r));
    if (plan(target, 20, source, 20, &cfg, &r, NULL) == 0 || r.status != CACHE_REUSE_ERR_BUDGET) {
        fprintf(stderr, "zero budget was not propagated\n");
        return 1;
    }
    if (!r.timing_known || r.elapsed_ns == 0 || r.preparation_ns == 0 ||
        r.discovery_entered || r.search_entered) {
        fprintf(stderr, "zero-budget timing was not preserved as measured preparation\n");
        return 1;
    }
    cfg = config_for(10);
    memset(&r, 0, sizeof(r));
    if (plan(target, 20, source, 20, &cfg, &r, always_cancel) == 0 ||
        r.status != CACHE_REUSE_ERR_CANCELLED) {
        fprintf(stderr, "cancel was not propagated\n");
        return 1;
    }
    puts("cache_reuse_utils_ffi_test: ok");
    return 0;
}
