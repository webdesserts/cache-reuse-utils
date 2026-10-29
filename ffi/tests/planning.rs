//! Coordinate, separator, candidate, reconciliation and schedule exports at
//! the C ABI boundary, called from Rust.

use cache_reuse_utils_ffi::*;
use std::ffi::{CStr, c_char};
use std::ptr;

/// Error buffer and a reader for its message.
struct Error([c_char; 128]);

impl Error {
    fn new() -> Self {
        Self([0x7f as c_char; 128])
    }
    fn ptr(&mut self) -> *mut c_char {
        self.0.as_mut_ptr()
    }
    fn message(&self) -> String {
        unsafe { CStr::from_ptr(self.0.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    }
}

/// Logical [5, 6, 7] with pads at physical 1 and 3.
const PHYSICAL: [i32; 5] = [5, -1, 6, -1, 7];
const PADS: [u32; 2] = [1, 3];

#[test]
fn verifies_logical_matches_and_reports_null_outputs() {
    let check = |logical: &[i32]| {
        let mut matches = true;
        let status = unsafe {
            cache_reuse_verify_matches_logical(
                PHYSICAL.as_ptr(),
                5,
                PADS.as_ptr(),
                2,
                logical.as_ptr(),
                logical.len() as u32,
                &mut matches,
                ptr::null_mut(),
                0,
            )
        };
        (status, matches)
    };
    assert_eq!(check(&[5, 6, 7]), (CACHE_REUSE_COMPLETE, true));
    assert_eq!(check(&[5, 6, 8]), (CACHE_REUSE_COMPLETE, false));

    let mut error = Error::new();
    let status = unsafe {
        cache_reuse_verify_matches_logical(
            PHYSICAL.as_ptr(),
            5,
            PADS.as_ptr(),
            2,
            ptr::null(),
            3,
            ptr::null_mut(),
            error.ptr(),
            128,
        )
    };
    assert_eq!(
        (status, error.message().as_str()),
        (CACHE_REUSE_ERR_INVALID, "null match output")
    );
    let mut matches = true;
    let status = unsafe {
        cache_reuse_verify_matches_logical(
            PHYSICAL.as_ptr(),
            5,
            PADS.as_ptr(),
            2,
            ptr::null(),
            3,
            &mut matches,
            error.ptr(),
            128,
        )
    };
    assert_eq!((status, matches), (CACHE_REUSE_ERR_INVALID, false));
    assert_eq!(error.message(), "logical pointer is null");
}

#[test]
fn slices_appends_and_truncates_pad_positions() {
    let pads = [1u32, 3, 6];
    let mut out = [u32::MAX; 3];
    let mut len = u32::MAX;
    let status = unsafe {
        cache_reuse_slice_pad_positions(
            pads.as_ptr(),
            3,
            8,
            1,
            6,
            out.as_mut_ptr(),
            3,
            &mut len,
            ptr::null_mut(),
            0,
        )
    };
    assert_eq!((status, len, out), (CACHE_REUSE_COMPLETE, 3, [0, 2, 5]));
    let mut error = Error::new();
    let status = unsafe {
        cache_reuse_slice_pad_positions(
            pads.as_ptr(),
            3,
            8,
            1,
            6,
            out.as_mut_ptr(),
            2,
            &mut len,
            error.ptr(),
            128,
        )
    };
    assert_eq!((status, len), (CACHE_REUSE_ERR_INVALID, 0));
    assert!(error.message().contains("too small"));

    let mut physical_len = 0;
    let status = unsafe {
        cache_reuse_append_unpadded(
            5,
            PADS.as_ptr(),
            2,
            3,
            &mut physical_len,
            ptr::null_mut(),
            0,
        )
    };
    assert_eq!((status, physical_len), (CACHE_REUSE_COMPLETE, 8));
    let status = unsafe {
        cache_reuse_append_unpadded(
            u32::MAX,
            ptr::null(),
            0,
            1,
            &mut physical_len,
            ptr::null_mut(),
            0,
        )
    };
    assert_eq!((status, physical_len), (CACHE_REUSE_ERR_INVALID, 0));

    let mut kept = u32::MAX;
    let status = unsafe {
        cache_reuse_truncate_pad_positions(PADS.as_ptr(), 2, 5, 3, &mut kept, ptr::null_mut(), 0)
    };
    assert_eq!((status, kept), (CACHE_REUSE_COMPLETE, 1));
    let status = unsafe {
        cache_reuse_truncate_pad_positions(PADS.as_ptr(), 2, 5, 6, &mut kept, ptr::null_mut(), 0)
    };
    assert_eq!((status, kept), (CACHE_REUSE_ERR_INVALID, 0));
}

#[test]
fn verifies_span_correspondence() {
    let check = |destination_pads: &[u32]| {
        let range = CacheReuseRange {
            source_start: 2,
            destination_start: 0,
            count: 5,
            prepared_start: 10,
        };
        let source = [3u32, 5];
        let mut corresponds = false;
        let status = unsafe {
            cache_reuse_verify_span_correspondence(
                source.as_ptr(),
                2,
                10,
                destination_pads.as_ptr(),
                destination_pads.len() as u32,
                20,
                &range,
                &mut corresponds,
                ptr::null_mut(),
                0,
            )
        };
        (status, corresponds)
    };
    assert_eq!(check(&[11, 13]), (CACHE_REUSE_COMPLETE, true));
    assert_eq!(check(&[11, 14]), (CACHE_REUSE_COMPLETE, false));
    let mut corresponds = true;
    let status = unsafe {
        cache_reuse_verify_span_correspondence(
            ptr::null(),
            0,
            10,
            ptr::null(),
            0,
            20,
            ptr::null(),
            &mut corresponds,
            ptr::null_mut(),
            0,
        )
    };
    assert_eq!((status, corresponds), (CACHE_REUSE_ERR_INVALID, false));
}

#[test]
fn collects_and_verifies_separator_boundaries() {
    let logical = [7, 0, 8, 1, 9];
    let separators = [0, 1];
    let mut out = [u32::MAX; 3];
    let mut len = 0;
    let status = unsafe {
        cache_reuse_collect_separator_boundaries(
            logical.as_ptr(),
            5,
            0,
            separators.as_ptr(),
            2,
            out.as_mut_ptr(),
            3,
            &mut len,
            ptr::null_mut(),
            0,
        )
    };
    assert_eq!((status, len, out), (CACHE_REUSE_COMPLETE, 3, [1, 2, 3]));

    // A pad (token 0) after the first space, and one at the end, which is no boundary.
    let check = |physical: &[i32], pads: &[u32]| {
        let mut authorized = false;
        let status = unsafe {
            cache_reuse_verify_separator_map(
                logical.as_ptr(),
                5,
                physical.as_ptr(),
                physical.len() as u32,
                pads.as_ptr(),
                pads.len() as u32,
                0,
                ptr::null(),
                0,
                0,
                separators.as_ptr(),
                2,
                &mut authorized,
                ptr::null_mut(),
                0,
            )
        };
        (status, authorized)
    };
    assert_eq!(
        check(&[7, 0, 0, 8, 1, 9], &[2]),
        (CACHE_REUSE_COMPLETE, true)
    );
    assert_eq!(
        check(&[7, 0, 8, 1, 9, 0], &[5]),
        (CACHE_REUSE_COMPLETE, false)
    );
}

fn candidate(tokens: &[i32], planned: u32) -> CacheReuseCandidate {
    CacheReuseCandidate {
        tokens: tokens.as_ptr(),
        token_len: tokens.len() as u32,
        pad_positions: ptr::null(),
        pad_len: 0,
        planned_reusable_tokens: planned,
    }
}

#[test]
fn selects_a_candidate() {
    let target: Vec<i32> = (0..20).collect();
    let hot_tokens: Vec<i32> = (0..8).collect();
    let pool_tokens: Vec<i32> = (0..4).collect();
    let policy = CacheReuseCandidatePolicy {
        minimum_match_tokens: 4,
        alignment: 2,
        restore_penalty_tokens: 3,
    };
    let hot = candidate(&hot_tokens, 0);
    // The second pool entry plans 16 reusable tokens.
    let pool = [candidate(&pool_tokens, 0), candidate(&pool_tokens, 16)];
    let select = |hot: *const CacheReuseCandidate, pool: &[CacheReuseCandidate]| {
        let mut out = CacheReuseCandidateSelection {
            action: 99,
            pool_index: 99,
            reusable_tokens: 99,
        };
        let status = unsafe {
            cache_reuse_select_candidate(
                target.as_ptr(),
                20,
                hot,
                pool.as_ptr(),
                pool.len() as u32,
                &policy,
                &mut out,
                ptr::null_mut(),
                0,
            )
        };
        (status, out)
    };
    let restore = CacheReuseCandidateSelection {
        action: CACHE_REUSE_ACTION_RESTORE_POOL,
        pool_index: 1,
        reusable_tokens: 16,
    };
    assert_eq!(select(&hot, &pool), (CACHE_REUSE_COMPLETE, restore));
    let keep = CacheReuseCandidateSelection {
        action: CACHE_REUSE_ACTION_KEEP_HOT,
        pool_index: 0,
        reusable_tokens: 8,
    };
    assert_eq!(select(&hot, &pool[..1]), (CACHE_REUSE_COMPLETE, keep));
    let fresh = CacheReuseCandidateSelection {
        action: CACHE_REUSE_ACTION_FRESH,
        ..Default::default()
    };
    assert_eq!(select(ptr::null(), &[]), (CACHE_REUSE_COMPLETE, fresh));
    // A pool candidate with a null token pointer is an error.
    let broken = [CacheReuseCandidate {
        tokens: ptr::null(),
        ..candidate(&pool_tokens, 0)
    }];
    assert_eq!(
        select(ptr::null(), &broken),
        (
            CACHE_REUSE_ERR_INVALID,
            CacheReuseCandidateSelection::default()
        )
    );
}

fn abi_range(source_start: u32, prepared_start: u32, count: u32) -> CacheReuseRange {
    CacheReuseRange {
        source_start,
        destination_start: prepared_start,
        count,
        prepared_start,
    }
}

fn same(a: &CacheReuseRange, b: &CacheReuseRange) -> bool {
    (
        a.source_start,
        a.destination_start,
        a.count,
        a.prepared_start,
    ) == (
        b.source_start,
        b.destination_start,
        b.count,
        b.prepared_start,
    )
}

#[test]
fn clips_and_reconciles_ranges() {
    let ranges = [
        abi_range(0, 8, 16),
        abi_range(40, 34, 16),
        abi_range(80, 64, 8),
    ];
    let mut out = [CacheReuseRange::default(); 3];
    let mut len = 0;
    let status = unsafe {
        cache_reuse_clip_ranges_to_archive(
            ranges.as_ptr(),
            3,
            0,
            1000,
            43,
            4,
            out.as_mut_ptr(),
            3,
            &mut len,
            ptr::null_mut(),
            0,
        )
    };
    assert_eq!((status, len), (CACHE_REUSE_COMPLETE, 2));
    assert!(same(&out[0], &ranges[0]) && same(&out[1], &abi_range(40, 34, 4)));

    let executed = [ranges[0], abi_range(80, 64, 4)];
    let mut summary = CacheReuseExecutionSummary::default();
    let status = unsafe {
        cache_reuse_reconcile_executed_ranges(
            ranges.as_ptr(),
            3,
            executed.as_ptr(),
            2,
            80,
            2,
            &mut summary,
            ptr::null_mut(),
            0,
        )
    };
    assert_eq!(status, CACHE_REUSE_COMPLETE);
    assert_eq!(
        summary,
        CacheReuseExecutionSummary {
            reused_tokens: 20,
            gap_tokens: 40,
            suffix_tokens: 12,
            fresh_tokens: 60,
            first_reused_start: 8,
            covered_end: 68,
        }
    );
    let moved = [abi_range(2, 8, 16)];
    let status = unsafe {
        cache_reuse_reconcile_executed_ranges(
            ranges.as_ptr(),
            3,
            moved.as_ptr(),
            1,
            80,
            2,
            &mut summary,
            ptr::null_mut(),
            0,
        )
    };
    assert_eq!(
        (status, summary),
        (
            CACHE_REUSE_ERR_INVALID,
            CacheReuseExecutionSummary::default()
        )
    );
}

#[test]
fn compiles_an_archive_schedule_and_reports_the_capacity_needed() {
    let geometry = CacheReuseArchiveGeometry {
        raw_arena_tokens: 8,
        max_import_chunk: 4,
        encoder_acquire_threshold: 0,
        encoder_release_tail: 0,
        head_alignment: 4,
    };
    let ranges = [abi_range(0, 4, 4)];
    let compile = |out: &mut [CacheReuseArchiveOp]| {
        let mut len = u32::MAX;
        let status = unsafe {
            cache_reuse_compile_archive_schedule(
                10,
                ranges.as_ptr(),
                1,
                &geometry,
                out.as_mut_ptr(),
                out.len() as u32,
                &mut len,
                ptr::null_mut(),
                0,
            )
        };
        (status, len)
    };
    let mut small = [CacheReuseArchiveOp::default(); 2];
    assert_eq!(compile(&mut small), (CACHE_REUSE_ERR_INVALID, 6));
    let mut ops = [CacheReuseArchiveOp::default(); 8];
    assert_eq!(compile(&mut ops), (CACHE_REUSE_COMPLETE, 6));
    let op = |kind, source_start, position, count, flush_arena| CacheReuseArchiveOp {
        kind,
        source_start,
        source_end: 0,
        position,
        count,
        flush_arena,
        defer_decoder: false,
    };
    assert_eq!(
        ops[..6],
        [
            op(CACHE_REUSE_OP_PREFILL_CHUNK, 0, 0, 4, true),
            op(CACHE_REUSE_OP_STAGE_SPAN, 0, 4, 4, false),
            op(CACHE_REUSE_OP_IMPORT_CHUNK, 0, 4, 4, true),
            op(CACHE_REUSE_OP_COVERED_HEAD, 0, 8, 0, false),
            op(CACHE_REUSE_OP_PREFILL_CHUNK, 0, 8, 2, false),
            op(CACHE_REUSE_OP_FINAL_HEAD, 0, 10, 0, false),
        ]
    );
    let mut len = u32::MAX;
    let status = unsafe {
        cache_reuse_compile_archive_schedule(
            10,
            ranges.as_ptr(),
            1,
            ptr::null(),
            ops.as_mut_ptr(),
            8,
            &mut len,
            ptr::null_mut(),
            0,
        )
    };
    assert_eq!((status, len), (CACHE_REUSE_ERR_INVALID, 0));
}
