//! `cache_reuse_plan` behavior at the C ABI boundary, called from Rust.

use cache_reuse_utils_ffi::{
    CACHE_REUSE_COMPLETE, CACHE_REUSE_ERR_CANCELLED, CACHE_REUSE_ERR_INVALID, CacheReuseConfig,
    CacheReuseResult, cache_reuse_plan,
};
use std::ffi::{CStr, c_char, c_void};
use std::ptr;

fn config() -> CacheReuseConfig {
    CacheReuseConfig {
        max_ranges: 1,
        minimum_match_tokens: 4,
        minimum_physical_reuse_tokens: 1,
        source_start_alignment: 1,
        prepared_target_start_alignment: 1,
        reuse_count_alignment: 1,
        minimum_prepared_reuse_start: 0,
        protected_prefix_tokens: 0,
        fresh_suffix_tokens: 0,
        max_trim_left: 0,
        max_trim_right: 0,
        pad_token: -1,
        loss_basis_points: 1000,
        max_new_padding: 0,
        max_prepared_tokens: 6,
        variant_limit: 0,
        budget_ms: 5000,
        allow_all_insertions: false,
        allowed_insertions: ptr::null(),
        allowed_insertion_count: 0,
        pad_penalty_tokens: 0,
    }
}

const TARGET: [i32; 6] = [1, 2, 3, 4, 5, 9];
const SOURCE: [i32; 4] = [2, 3, 4, 5];

/// Run a plan and return the return code, result and error text.
fn plan(
    config: Option<&CacheReuseConfig>,
    error_len: usize,
    cancel: Option<unsafe extern "C" fn(*mut c_void) -> bool>,
) -> (i32, CacheReuseResult, String) {
    // Garbage the caller might pass in: every field must be overwritten.
    let mut result = CacheReuseResult {
        status: 999,
        range_count: 999,
        ..Default::default()
    };
    let mut error = [0x7f as c_char; 64];
    let code = unsafe {
        cache_reuse_plan(
            TARGET.as_ptr(),
            TARGET.len() as u32,
            SOURCE.as_ptr(),
            SOURCE.len() as u32,
            1,
            config.map_or(ptr::null(), ptr::from_ref),
            &mut result,
            error.as_mut_ptr(),
            error_len,
            cancel,
            ptr::null_mut(),
        )
    };
    let text = if error_len == 0 {
        String::new()
    } else {
        unsafe { CStr::from_ptr(error.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    };
    (code, result, text)
}

#[test]
fn a_null_config_still_initializes_the_result() {
    let (code, result, error) = plan(None, 64, None);
    assert_eq!(code, 1);
    assert_eq!(result.status, CACHE_REUSE_ERR_INVALID);
    assert_eq!(result.range_count, 0);
    assert_eq!(error, "null planner config");
}

#[test]
fn allow_all_insertions_permits_any_boundary() {
    let mut config = config();
    config.prepared_target_start_alignment = 2;
    config.pad_token = 77;
    config.max_new_padding = 1;
    config.max_prepared_tokens = 7;
    config.allow_all_insertions = true;
    let (code, result, _) = plan(Some(&config), 64, None);
    assert_eq!(code, 0);
    assert_eq!(result.status, CACHE_REUSE_COMPLETE);
    assert_eq!(result.insertion_count, 1);
    assert_eq!(result.insertions[0].before_target_index, 1);
    assert_eq!(result.ranges[0].prepared_start, 2);
}

#[test]
fn the_pad_penalty_defaults_to_sixteen_when_padding_is_enabled() {
    let mut config = config();
    config.prepared_target_start_alignment = 2;
    config.pad_token = 77;
    config.max_new_padding = 1;
    config.max_prepared_tokens = 7;
    config.allow_all_insertions = true;
    config.loss_basis_points = 10_000;
    // Four covered tokens do not repay the default 16-token pad cost.
    let (_, result, _) = plan(Some(&config), 64, None);
    assert_eq!(result.range_count, 0);
    config.pad_penalty_tokens = 1;
    let (_, result, _) = plan(Some(&config), 64, None);
    assert_eq!(result.covered_tokens, 4);
}

#[test]
fn error_text_is_truncated_and_terminated() {
    let mut config = config();
    config.max_ranges = 0;
    let (code, _, error) = plan(Some(&config), 8, None);
    assert_eq!(code, 1);
    assert_eq!(error, "invalid");
    let (code, result, _) = plan(Some(&config), 0, None);
    assert_eq!((code, result.status), (1, CACHE_REUSE_ERR_INVALID));
}

unsafe extern "C" fn cancel_now(_: *mut c_void) -> bool {
    true
}

#[test]
fn cancellation_is_reported_with_timing() {
    let (code, result, error) = plan(Some(&config()), 64, Some(cancel_now));
    assert_eq!(code, 1);
    assert_eq!(result.status, CACHE_REUSE_ERR_CANCELLED);
    assert!(result.timing_known);
    assert!(error.starts_with("cancelled"));
}
