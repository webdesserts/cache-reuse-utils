//! `cache_reuse_materialize` and the coordinate helpers at the C ABI.

use cache_reuse_utils_ffi::{
    CACHE_REUSE_COMPLETE, CACHE_REUSE_ERR_INVALID, CacheReuseCommonPrefix, CacheReuseConfig,
    CacheReuseResult, cache_reuse_common_prefix, cache_reuse_logical_boundary,
    cache_reuse_materialize, cache_reuse_plan, cache_reuse_project_logical,
};
use std::ffi::{CStr, c_char};
use std::ptr;

const TARGET: [i32; 6] = [1, 2, 3, 4, 5, 9];
const SOURCE: [i32; 4] = [2, 3, 4, 5];
const PAD: i32 = 77;

/// A plan that pads one token before `SOURCE` to reach an even start.
fn padded_plan() -> CacheReuseResult {
    let config = CacheReuseConfig {
        max_ranges: 1,
        minimum_match_tokens: 4,
        minimum_physical_reuse_tokens: 4,
        source_start_alignment: 1,
        prepared_target_start_alignment: 2,
        reuse_count_alignment: 1,
        minimum_prepared_reuse_start: 0,
        protected_prefix_tokens: 0,
        fresh_suffix_tokens: 0,
        max_trim_left: 0,
        max_trim_right: 0,
        pad_token: PAD,
        loss_basis_points: 1000,
        max_new_padding: 1,
        max_prepared_tokens: 7,
        variant_limit: 0,
        budget_ms: 5000,
        allow_all_insertions: true,
        allowed_insertions: ptr::null(),
        allowed_insertion_count: 0,
        pad_penalty_tokens: 1,
        range_penalty_tokens: 0,
    };
    let mut result = CacheReuseResult::default();
    let code = unsafe {
        cache_reuse_plan(
            TARGET.as_ptr(),
            TARGET.len() as u32,
            SOURCE.as_ptr(),
            SOURCE.len() as u32,
            1,
            &config,
            &mut result,
            ptr::null_mut(),
            0,
            None,
            ptr::null_mut(),
        )
    };
    assert_eq!(code, 0);
    result
}

struct Materialized {
    status: u32,
    prepared: Vec<i32>,
    pads: Vec<u32>,
    error: String,
}

/// Call `cache_reuse_materialize` with buffers of the given lengths.
/// `pad_len: None` passes a null pad buffer of length 0.
fn materialize(
    target: &[i32],
    source: &[i32],
    result: Option<&CacheReuseResult>,
    prepared_len: usize,
    pad_len: Option<usize>,
) -> Materialized {
    let mut prepared = vec![-7; prepared_len];
    let mut pads = vec![u32::MAX; pad_len.unwrap_or(0)];
    let mut error = [0 as c_char; 96];
    let status = unsafe {
        cache_reuse_materialize(
            target.as_ptr(),
            target.len() as u32,
            source.as_ptr(),
            source.len() as u32,
            result.map_or(ptr::null(), ptr::from_ref),
            prepared.as_mut_ptr(),
            prepared_len as u32,
            if pad_len.is_some() {
                pads.as_mut_ptr()
            } else {
                ptr::null_mut()
            },
            pads.len() as u32,
            error.as_mut_ptr(),
            error.len(),
        )
    };
    let error = unsafe { CStr::from_ptr(error.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    Materialized {
        status,
        prepared,
        pads,
        error,
    }
}

#[test]
fn plan_results_carry_insertion_prepared_starts_and_discovery_stats() {
    let result = padded_plan();
    assert_eq!(result.insertion_count, 1);
    let insertion = result.insertions[0];
    assert_eq!(
        (insertion.before_target_index, insertion.prepared_start),
        (1, 1)
    );
    assert_eq!(result.discovered_match_count, 1);
    assert_eq!(result.longest_match_tokens, 4);
}

#[test]
fn a_plan_materializes_into_caller_buffers_with_pad_positions() {
    let result = padded_plan();
    let out = materialize(&TARGET, &SOURCE, Some(&result), 7, Some(1));
    assert_eq!((out.status, out.error.as_str()), (CACHE_REUSE_COMPLETE, ""));
    assert_eq!(out.prepared, [1, PAD, 2, 3, 4, 5, 9]);
    assert_eq!(out.pads, [1]);

    let out = materialize(&TARGET, &SOURCE, Some(&result), 7, None);
    assert_eq!(out.status, CACHE_REUSE_COMPLETE);
    assert_eq!(out.prepared, [1, PAD, 2, 3, 4, 5, 9]);
}

#[test]
fn materialize_rejects_inconsistent_inputs() {
    let result = padded_plan();
    let invalid = |out: Materialized, message: &str| {
        assert_eq!(out.status, CACHE_REUSE_ERR_INVALID, "{message}");
        assert_eq!(out.error, message);
    };
    invalid(
        materialize(&TARGET, &SOURCE, None, 7, None),
        "null planner result",
    );
    let shape = "output length does not match the result";
    invalid(materialize(&TARGET, &SOURCE, Some(&result), 6, None), shape);
    invalid(
        materialize(&TARGET, &SOURCE, Some(&result), 7, Some(2)),
        shape,
    );
    invalid(
        materialize(&TARGET[..5], &SOURCE, Some(&result), 7, None),
        "target length does not match the result",
    );
    invalid(
        materialize(&[1, 2, 3, 4, -5, 9], &SOURCE, Some(&result), 7, None),
        "target contains a negative token",
    );
    invalid(
        materialize(&TARGET, &[2, 3, 4, 6], Some(&result), 7, None),
        "source token differs from target token 4",
    );

    let mut stale = padded_plan();
    stale.insertions[0].prepared_start = 0;
    invalid(
        materialize(&TARGET, &SOURCE, Some(&stale), 7, None),
        "result recipe is inconsistent",
    );
    let mut failed = padded_plan();
    failed.status = CACHE_REUSE_ERR_INVALID;
    invalid(
        materialize(&TARGET, &SOURCE, Some(&failed), 7, None),
        "result does not hold a plan",
    );
    let mut oversized = padded_plan();
    oversized.range_count = 33;
    invalid(
        materialize(&TARGET, &SOURCE, Some(&oversized), 7, None),
        "result counts exceed ABI bounds",
    );
}

#[test]
fn a_null_pad_buffer_with_a_length_is_rejected() {
    let result = padded_plan();
    let mut prepared = [0i32; 7];
    let status = unsafe {
        cache_reuse_materialize(
            TARGET.as_ptr(),
            6,
            SOURCE.as_ptr(),
            4,
            &result,
            prepared.as_mut_ptr(),
            7,
            ptr::null_mut(),
            1,
            ptr::null_mut(),
            0,
        )
    };
    assert_eq!(status, CACHE_REUSE_ERR_INVALID);
}

/// Logical `[5, -6, 7]` with pads at physical 1 and 3. Coordinate helpers
/// treat token values as opaque, so negative tokens are allowed.
const PHYSICAL: [i32; 5] = [5, 0, -6, 0, 7];
const PADS: [u32; 2] = [1, 3];

#[test]
fn projection_writes_logical_tokens_and_their_count() {
    let mut logical = [0i32; 4];
    let mut written = 99;
    let status = unsafe {
        cache_reuse_project_logical(
            PHYSICAL.as_ptr(),
            5,
            PADS.as_ptr(),
            2,
            logical.as_mut_ptr(),
            4,
            &mut written,
            ptr::null_mut(),
            0,
        )
    };
    assert_eq!((status, written), (CACHE_REUSE_COMPLETE, 3));
    assert_eq!(logical[..3], [5, -6, 7]);

    let status = unsafe {
        cache_reuse_project_logical(
            PHYSICAL.as_ptr(),
            5,
            PADS.as_ptr(),
            2,
            logical.as_mut_ptr(),
            2,
            &mut written,
            ptr::null_mut(),
            0,
        )
    };
    assert_eq!((status, written), (CACHE_REUSE_ERR_INVALID, 0));
}

#[test]
fn boundaries_map_and_invalid_pads_are_rejected() {
    let boundary = |physical, pads: &[u32]| {
        let mut logical = 99;
        let status = unsafe {
            cache_reuse_logical_boundary(
                physical,
                5,
                pads.as_ptr(),
                pads.len() as u32,
                &mut logical,
                ptr::null_mut(),
                0,
            )
        };
        (status, logical)
    };
    assert_eq!(boundary(4, &PADS), (CACHE_REUSE_COMPLETE, 2));
    assert_eq!(boundary(5, &PADS), (CACHE_REUSE_COMPLETE, 3));
    assert_eq!(boundary(6, &PADS), (CACHE_REUSE_ERR_INVALID, 0));
    assert_eq!(boundary(2, &[3, 1]), (CACHE_REUSE_ERR_INVALID, 0));
    let status =
        unsafe { cache_reuse_logical_boundary(1, 5, ptr::null(), 2, &mut 0, ptr::null_mut(), 0) };
    assert_eq!(status, CACHE_REUSE_ERR_INVALID);
}

#[test]
fn common_prefix_reports_carried_pads() {
    let logical = [5, -6, 8];
    let mut out = CacheReuseCommonPrefix::default();
    let status = unsafe {
        cache_reuse_common_prefix(
            logical.as_ptr(),
            3,
            PHYSICAL.as_ptr(),
            5,
            PADS.as_ptr(),
            2,
            &mut out,
            ptr::null_mut(),
            0,
        )
    };
    assert_eq!(status, CACHE_REUSE_COMPLETE);
    assert_eq!(
        out,
        CacheReuseCommonPrefix {
            common_logical_tokens: 2,
            physical_prefix_tokens: 3,
            carried_pads: 1,
            live_logical_tokens: 3,
        }
    );
    let status = unsafe {
        cache_reuse_common_prefix(
            logical.as_ptr(),
            3,
            PHYSICAL.as_ptr(),
            5,
            PADS.as_ptr(),
            2,
            ptr::null_mut(),
            ptr::null_mut(),
            0,
        )
    };
    assert_eq!(status, CACHE_REUSE_ERR_INVALID);
}
