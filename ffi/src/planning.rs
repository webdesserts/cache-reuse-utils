//! C ABI for the stateless coordinate, separator, candidate, reconciliation
//! and archive-schedule helpers.
//!
//! Every function returns `CACHE_REUSE_COMPLETE`, `CACHE_REUSE_ERR_INVALID`
//! (with a message) or `CACHE_REUSE_ERR_PANIC`. A failed check is a result,
//! not an error: `verify` functions report it through their `bool` output.
//! Outputs are reset before validation, so they are zero or false on error.

use std::ffi::c_char;

use cache_reuse_utils::{
    ArchiveGeometry, ArchiveOp, CandidateAction, CandidateInput, CandidateSelectionPolicy,
    PlacedRange,
};

use crate::{CacheReuseRange, borrow_out, borrow_tokens, borrow_u32, run_status};

/// `CacheReuseCandidateSelection::action`: no candidate is worth reusing.
pub const CACHE_REUSE_ACTION_FRESH: u32 = 0;
/// `CacheReuseCandidateSelection::action`: keep the hot candidate.
pub const CACHE_REUSE_ACTION_KEEP_HOT: u32 = 1;
/// `CacheReuseCandidateSelection::action`: restore pool entry `pool_index`.
pub const CACHE_REUSE_ACTION_RESTORE_POOL: u32 = 2;

/// `CacheReuseArchiveOp::kind` values; see the core `ArchiveOp`.
pub const CACHE_REUSE_OP_ACQUIRE_ENCODER: u32 = 0;
pub const CACHE_REUSE_OP_RELEASE_ENCODER: u32 = 1;
pub const CACHE_REUSE_OP_PREFETCH_WINDOW: u32 = 2;
pub const CACHE_REUSE_OP_STAGE_SPAN: u32 = 3;
pub const CACHE_REUSE_OP_IMPORT_CHUNK: u32 = 4;
pub const CACHE_REUSE_OP_PREFILL_CHUNK: u32 = 5;
pub const CACHE_REUSE_OP_COVERED_HEAD: u32 = 6;
pub const CACHE_REUSE_OP_FINAL_HEAD: u32 = 7;

/// One cached sequence for `cache_reuse_select_candidate`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CacheReuseCandidate {
    /// Physical tokens, pads included.
    pub tokens: *const i32,
    pub token_len: u32,
    /// Strictly increasing pad positions in `tokens`.
    pub pad_positions: *const u32,
    pub pad_len: u32,
    /// Target tokens a placement plan reuses from this candidate, or 0.
    pub planned_reusable_tokens: u32,
}

/// Field meanings match the core `CandidateSelectionPolicy`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct CacheReuseCandidatePolicy {
    pub minimum_match_tokens: u32,
    /// Nonzero.
    pub alignment: u32,
    pub restore_penalty_tokens: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct CacheReuseCandidateSelection {
    /// One of the `CACHE_REUSE_ACTION_*` values.
    pub action: u32,
    /// The pool entry to restore; 0 unless `action` is
    /// `CACHE_REUSE_ACTION_RESTORE_POOL`.
    pub pool_index: u32,
    pub reusable_tokens: u32,
}

/// Field meanings match the core `ExecutionSummary`.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct CacheReuseExecutionSummary {
    pub reused_tokens: u32,
    pub gap_tokens: u32,
    pub suffix_tokens: u32,
    pub fresh_tokens: u32,
    pub first_reused_start: u32,
    pub covered_end: u32,
}

/// Field meanings match the core `ArchiveGeometry`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct CacheReuseArchiveGeometry {
    pub raw_arena_tokens: u32,
    pub max_import_chunk: u32,
    pub encoder_acquire_threshold: u32,
    pub encoder_release_tail: u32,
    pub head_alignment: u32,
}

/// One schedule step. Unused fields are 0 or false.
///
/// - `PREFETCH_WINDOW`: source window `[source_start, source_end)`.
/// - `STAGE_SPAN`, `IMPORT_CHUNK`: `count` tokens from `source_start` to
///   destination `position`; imports may `flush_arena`.
/// - `PREFILL_CHUNK`: `count` tokens from `position`, with `flush_arena`
///   and `defer_decoder`.
/// - `COVERED_HEAD`, `FINAL_HEAD`: the head at `position`.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct CacheReuseArchiveOp {
    /// One of the `CACHE_REUSE_OP_*` values.
    pub kind: u32,
    pub source_start: u32,
    pub source_end: u32,
    pub position: u32,
    pub count: u32,
    pub flush_arena: bool,
    pub defer_decoder: bool,
}

fn placed(range: &CacheReuseRange) -> PlacedRange {
    PlacedRange {
        source_start: range.source_start,
        destination_start: range.destination_start,
        token_count: range.count,
        prepared_start: range.prepared_start,
    }
}

fn abi_range(range: PlacedRange) -> CacheReuseRange {
    CacheReuseRange {
        source_start: range.source_start,
        destination_start: range.destination_start,
        count: range.token_count,
        prepared_start: range.prepared_start,
    }
}

/// Copy `len` caller ranges into core ranges.
unsafe fn borrow_ranges(
    pointer: *const CacheReuseRange,
    len: u32,
    what: &str,
) -> Result<Vec<PlacedRange>, String> {
    if len == 0 {
        return Ok(Vec::new());
    }
    if pointer.is_null() {
        return Err(format!("{what} pointer is null"));
    }
    let ranges = unsafe { std::slice::from_raw_parts(pointer, len as usize) };
    Ok(ranges.iter().map(placed).collect())
}

/// Reset `*out` and return it, or fail for a null pointer.
unsafe fn reset<'a, T: Default>(out: *mut T, what: &str) -> Result<&'a mut T, String> {
    if out.is_null() {
        return Err(format!("null {what} output"));
    }
    let out = unsafe { &mut *out };
    *out = T::default();
    Ok(out)
}

fn abi_op(op: ArchiveOp) -> CacheReuseArchiveOp {
    let base = CacheReuseArchiveOp::default();
    match op {
        ArchiveOp::AcquireEncoderResidency => CacheReuseArchiveOp {
            kind: CACHE_REUSE_OP_ACQUIRE_ENCODER,
            ..base
        },
        ArchiveOp::ReleaseEncoderResidency => CacheReuseArchiveOp {
            kind: CACHE_REUSE_OP_RELEASE_ENCODER,
            ..base
        },
        ArchiveOp::PrefetchWindow {
            covered_start,
            range_end,
        } => CacheReuseArchiveOp {
            kind: CACHE_REUSE_OP_PREFETCH_WINDOW,
            source_start: covered_start,
            source_end: range_end,
            ..base
        },
        ArchiveOp::StageSpan {
            source_start,
            destination_start,
            count,
        } => CacheReuseArchiveOp {
            kind: CACHE_REUSE_OP_STAGE_SPAN,
            source_start,
            position: destination_start,
            count,
            ..base
        },
        ArchiveOp::ImportRebaseChunk {
            source_start,
            destination_start,
            count,
            flush_arena,
        } => CacheReuseArchiveOp {
            kind: CACHE_REUSE_OP_IMPORT_CHUNK,
            source_start,
            position: destination_start,
            count,
            flush_arena,
            ..base
        },
        ArchiveOp::PrefillChunk {
            start,
            count,
            flush_arena,
            defer_decoder,
        } => CacheReuseArchiveOp {
            kind: CACHE_REUSE_OP_PREFILL_CHUNK,
            position: start,
            count,
            flush_arena,
            defer_decoder,
            ..base
        },
        ArchiveOp::ComputeCoveredHead { covered_end } => CacheReuseArchiveOp {
            kind: CACHE_REUSE_OP_COVERED_HEAD,
            position: covered_end,
            ..base
        },
        ArchiveOp::ComputeFinalHead { physical_len } => CacheReuseArchiveOp {
            kind: CACHE_REUSE_OP_FINAL_HEAD,
            position: physical_len,
            ..base
        },
    }
}

/// Store in `*out_matches` whether removing the pads from the physical
/// sequence leaves the logical sequence, token for token. Invalid pad
/// positions are a mismatch, not an error.
///
/// # Safety
///
/// Each pointer must be valid for its stated length and may be null only for
/// a zero length; `out_matches` must be valid for one `bool`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cache_reuse_verify_matches_logical(
    physical_tokens: *const i32,
    physical_len: u32,
    pad_positions: *const u32,
    pad_len: u32,
    logical_tokens: *const i32,
    logical_len: u32,
    out_matches: *mut bool,
    error_out: *mut c_char,
    error_len: usize,
) -> u32 {
    run_status(error_out, error_len, "logical match", || {
        let out = unsafe { reset(out_matches, "match") }?;
        let physical = unsafe { borrow_tokens(physical_tokens, physical_len, "physical", false) }?;
        let pads = unsafe { borrow_u32(pad_positions, pad_len, "pad positions") }?;
        let logical = unsafe { borrow_tokens(logical_tokens, logical_len, "logical", false) }?;
        *out = cache_reuse_utils::verify_matches_logical(physical, pads, logical);
        Ok(())
    })
}

/// Write the pad positions inside `[slice_start, slice_start + slice_len)`
/// to `out_pads`, relative to `slice_start`, and store their number in
/// `*out_pad_len`.
///
/// # Safety
///
/// Each pointer must be valid for its stated length and may be null only for
/// a zero length; `out_pad_len` must be valid for one `u32`. Output buffers
/// must not overlap any input.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cache_reuse_slice_pad_positions(
    pad_positions: *const u32,
    pad_len: u32,
    physical_len: u32,
    slice_start: u32,
    slice_len: u32,
    out_pads: *mut u32,
    out_pad_capacity: u32,
    out_pad_len: *mut u32,
    error_out: *mut c_char,
    error_len: usize,
) -> u32 {
    run_status(error_out, error_len, "pad slicing", || {
        let written = unsafe { reset(out_pad_len, "pad length") }?;
        let pads = unsafe { borrow_u32(pad_positions, pad_len, "pad positions") }?;
        let out = unsafe { borrow_out(out_pads, out_pad_capacity, "pad output") }?;
        *written =
            cache_reuse_utils::slice_pad_positions(pads, physical_len, slice_start, slice_len, out)
                .ok_or(
                    "invalid pad positions, slice beyond the sequence, or pad output too small",
                )?;
        Ok(())
    })
}

/// Store in `*out_physical_len` the physical length after appending
/// `appended_tokens` non-pad tokens.
///
/// # Safety
///
/// `pad_positions` must be valid for `pad_len` values and may be null only
/// for a zero length; `out_physical_len` must be valid for one `u32`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cache_reuse_append_unpadded(
    physical_len: u32,
    pad_positions: *const u32,
    pad_len: u32,
    appended_tokens: u32,
    out_physical_len: *mut u32,
    error_out: *mut c_char,
    error_len: usize,
) -> u32 {
    run_status(error_out, error_len, "append", || {
        let out = unsafe { reset(out_physical_len, "physical length") }?;
        let pads = unsafe { borrow_u32(pad_positions, pad_len, "pad positions") }?;
        *out = cache_reuse_utils::append_unpadded(physical_len, pads, appended_tokens)
            .ok_or("invalid pad positions or physical length overflow")?;
        Ok(())
    })
}

/// Store in `*out_pad_len` how many leading pad positions survive
/// truncation to `new_physical_len`.
///
/// # Safety
///
/// `pad_positions` must be valid for `pad_len` values and may be null only
/// for a zero length; `out_pad_len` must be valid for one `u32`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cache_reuse_truncate_pad_positions(
    pad_positions: *const u32,
    pad_len: u32,
    physical_len: u32,
    new_physical_len: u32,
    out_pad_len: *mut u32,
    error_out: *mut c_char,
    error_len: usize,
) -> u32 {
    run_status(error_out, error_len, "truncation", || {
        let out = unsafe { reset(out_pad_len, "pad length") }?;
        let pads = unsafe { borrow_u32(pad_positions, pad_len, "pad positions") }?;
        let kept = cache_reuse_utils::truncate_pad_positions(pads, physical_len, new_physical_len)
            .ok_or("invalid pad positions or new length beyond the sequence")?;
        *out = kept as u32;
        Ok(())
    })
}

/// Store in `*out_corresponds` whether `range` copies the source pads to the
/// same offsets in the destination (`range->prepared_start`), with no pad on
/// its first or last token.
///
/// # Safety
///
/// Each pad pointer must be valid for its length and may be null only for a
/// zero length; `range` must be valid for one value and `out_corresponds`
/// for one `bool`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cache_reuse_verify_span_correspondence(
    source_pads: *const u32,
    source_pad_len: u32,
    source_len: u32,
    destination_pads: *const u32,
    destination_pad_len: u32,
    destination_len: u32,
    range: *const CacheReuseRange,
    out_corresponds: *mut bool,
    error_out: *mut c_char,
    error_len: usize,
) -> u32 {
    run_status(error_out, error_len, "span correspondence", || {
        let out = unsafe { reset(out_corresponds, "correspondence") }?;
        if range.is_null() {
            return Err("null range".into());
        }
        let range = placed(unsafe { &*range });
        let source = unsafe { borrow_u32(source_pads, source_pad_len, "source pads") }?;
        let destination =
            unsafe { borrow_u32(destination_pads, destination_pad_len, "destination pads") }?;
        *out = cache_reuse_utils::verify_span_correspondence(
            source,
            source_len,
            destination,
            destination_len,
            range,
        );
        Ok(())
    })
}

/// Write the separator boundaries at or after `start_index` to
/// `out_boundaries`, up to its capacity, and store their number in
/// `*out_boundary_len`.
///
/// # Safety
///
/// Each pointer must be valid for its stated length and may be null only for
/// a zero length; `out_boundary_len` must be valid for one `u32`. Output
/// buffers must not overlap any input.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cache_reuse_collect_separator_boundaries(
    logical_tokens: *const i32,
    logical_len: u32,
    start_index: u32,
    separator_tokens: *const i32,
    separator_len: u32,
    out_boundaries: *mut u32,
    out_boundary_capacity: u32,
    out_boundary_len: *mut u32,
    error_out: *mut c_char,
    error_len: usize,
) -> u32 {
    run_status(error_out, error_len, "separator boundaries", || {
        let written = unsafe { reset(out_boundary_len, "boundary length") }?;
        let logical = unsafe { borrow_tokens(logical_tokens, logical_len, "logical", false) }?;
        let separators =
            unsafe { borrow_tokens(separator_tokens, separator_len, "separators", false) }?;
        let out = unsafe { borrow_out(out_boundaries, out_boundary_capacity, "boundary output") }?;
        *written =
            cache_reuse_utils::collect_separator_boundaries(logical, start_index, separators, out);
        Ok(())
    })
}

/// Store in `*out_authorized` whether the physical sequence is the logical
/// one with the carried pads below `carried_physical_prefix` and otherwise
/// only `pad_token` pads at separator boundaries.
///
/// # Safety
///
/// Each pointer must be valid for its stated length and may be null only for
/// a zero length; `out_authorized` must be valid for one `bool`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cache_reuse_verify_separator_map(
    logical_tokens: *const i32,
    logical_len: u32,
    physical_tokens: *const i32,
    physical_len: u32,
    pad_positions: *const u32,
    pad_len: u32,
    carried_physical_prefix: u32,
    carried_pads: *const u32,
    carried_pad_len: u32,
    pad_token: i32,
    separator_tokens: *const i32,
    separator_len: u32,
    out_authorized: *mut bool,
    error_out: *mut c_char,
    error_len: usize,
) -> u32 {
    run_status(error_out, error_len, "separator map", || {
        let out = unsafe { reset(out_authorized, "authorization") }?;
        let logical = unsafe { borrow_tokens(logical_tokens, logical_len, "logical", false) }?;
        let physical = unsafe { borrow_tokens(physical_tokens, physical_len, "physical", false) }?;
        let pads = unsafe { borrow_u32(pad_positions, pad_len, "pad positions") }?;
        let carried = unsafe { borrow_u32(carried_pads, carried_pad_len, "carried pads") }?;
        let separators =
            unsafe { borrow_tokens(separator_tokens, separator_len, "separators", false) }?;
        *out = cache_reuse_utils::verify_separator_map(
            logical,
            physical,
            pads,
            carried_physical_prefix,
            carried,
            pad_token as u32,
            separators,
        );
        Ok(())
    })
}

unsafe fn candidate<'a>(
    input: &CacheReuseCandidate,
    what: &str,
) -> Result<CandidateInput<'a>, String> {
    Ok(CandidateInput {
        physical: unsafe { borrow_tokens(input.tokens, input.token_len, what, false) }?,
        pad_positions: unsafe { borrow_u32(input.pad_positions, input.pad_len, what) }?,
        planned_reusable_tokens: input.planned_reusable_tokens,
    })
}

/// Choose between keeping `hot` (may be null), restoring a `pool` entry or
/// starting fresh, storing the choice in `*out`.
///
/// # Safety
///
/// Token and pool pointers must be valid for their stated lengths and may be
/// null only for a zero length; every candidate's pointers likewise. `hot`
/// is null or valid for one value; `policy` and `out` must be valid for one
/// value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cache_reuse_select_candidate(
    target_tokens: *const i32,
    target_len: u32,
    hot: *const CacheReuseCandidate,
    pool: *const CacheReuseCandidate,
    pool_len: u32,
    policy: *const CacheReuseCandidatePolicy,
    out: *mut CacheReuseCandidateSelection,
    error_out: *mut c_char,
    error_len: usize,
) -> u32 {
    run_status(error_out, error_len, "candidate selection", || {
        let out = unsafe { reset(out, "selection") }?;
        if policy.is_null() {
            return Err("null policy".into());
        }
        let policy = unsafe { *policy };
        let target = unsafe { borrow_tokens(target_tokens, target_len, "target", false) }?;
        let hot = if hot.is_null() {
            None
        } else {
            Some(unsafe { candidate(&*hot, "hot candidate") }?)
        };
        let pool = if pool_len == 0 {
            &[][..]
        } else if pool.is_null() {
            return Err("pool pointer is null".into());
        } else {
            unsafe { std::slice::from_raw_parts(pool, pool_len as usize) }
        };
        let pool = pool
            .iter()
            .map(|entry| unsafe { candidate(entry, "pool candidate") })
            .collect::<Result<Vec<_>, _>>()?;
        let policy = CandidateSelectionPolicy {
            minimum_match_tokens: policy.minimum_match_tokens,
            alignment: policy.alignment,
            restore_penalty_tokens: policy.restore_penalty_tokens,
        };
        let selection = cache_reuse_utils::select_candidate(target, hot, &pool, policy)
            .ok_or("invalid candidate pad positions or zero alignment")?;
        let (action, pool_index) = match selection.action {
            CandidateAction::Fresh => (CACHE_REUSE_ACTION_FRESH, 0),
            CandidateAction::KeepHot => (CACHE_REUSE_ACTION_KEEP_HOT, 0),
            CandidateAction::RestorePool { index } => (CACHE_REUSE_ACTION_RESTORE_POOL, index),
        };
        *out = CacheReuseCandidateSelection {
            action,
            pool_index,
            reusable_tokens: selection.reusable_tokens,
        };
        Ok(())
    })
}

/// Clip ranges to the archive source `[archive_start, archive_end)` and the
/// aligned destination cap, writing survivors to `out_ranges` and their
/// number to `*out_range_len`.
///
/// # Safety
///
/// Each pointer must be valid for its stated length and may be null only for
/// a zero length; `out_range_len` must be valid for one `u32`. Output
/// buffers must not overlap any input.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cache_reuse_clip_ranges_to_archive(
    ranges: *const CacheReuseRange,
    range_len: u32,
    archive_start: u32,
    archive_end: u32,
    target_cap: u32,
    alignment: u32,
    out_ranges: *mut CacheReuseRange,
    out_range_capacity: u32,
    out_range_len: *mut u32,
    error_out: *mut c_char,
    error_len: usize,
) -> u32 {
    run_status(error_out, error_len, "range clipping", || {
        let written = unsafe { reset(out_range_len, "range length") }?;
        let ranges = unsafe { borrow_ranges(ranges, range_len, "ranges") }?;
        let out = unsafe { borrow_out(out_ranges, out_range_capacity, "range output") }?;
        let empty = PlacedRange {
            source_start: 0,
            destination_start: 0,
            token_count: 0,
            prepared_start: 0,
        };
        // Clipping never adds ranges, so this bounds the copy by the input.
        let mut clipped = vec![empty; out.len().min(ranges.len())];
        let count = cache_reuse_utils::clip_ranges_to_archive(
            &ranges,
            archive_start,
            archive_end,
            target_cap,
            alignment,
            &mut clipped,
        )
        .ok_or("invalid ranges, archive bounds or alignment, or range output too small")?;
        for (slot, &range) in out.iter_mut().zip(&clipped[..count as usize]) {
            *slot = abi_range(range);
        }
        *written = count;
        Ok(())
    })
}

/// Check that `executed` is an ordered, aligned subset of `planned` and
/// store its token accounting in `*out`.
///
/// # Safety
///
/// Each pointer must be valid for its stated length and may be null only for
/// a zero length; `out` must be valid for one value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cache_reuse_reconcile_executed_ranges(
    planned: *const CacheReuseRange,
    planned_len: u32,
    executed: *const CacheReuseRange,
    executed_len: u32,
    physical_len: u32,
    alignment: u32,
    out: *mut CacheReuseExecutionSummary,
    error_out: *mut c_char,
    error_len: usize,
) -> u32 {
    run_status(error_out, error_len, "reconciliation", || {
        let out = unsafe { reset(out, "summary") }?;
        let planned = unsafe { borrow_ranges(planned, planned_len, "planned ranges") }?;
        let executed = unsafe { borrow_ranges(executed, executed_len, "executed ranges") }?;
        let summary = cache_reuse_utils::reconcile_executed_ranges(
            &planned,
            &executed,
            physical_len,
            alignment,
        )
        .ok_or("executed ranges are not an aligned, ordered subset of a valid plan")?;
        *out = CacheReuseExecutionSummary {
            reused_tokens: summary.reused_tokens,
            gap_tokens: summary.gap_tokens,
            suffix_tokens: summary.suffix_tokens,
            fresh_tokens: summary.fresh_tokens,
            first_reused_start: summary.first_reused_start,
            covered_end: summary.covered_end,
        };
        Ok(())
    })
}

/// Compile the archive schedule for `physical_len` tokens and `ranges` into
/// `out_ops`, storing the op count in `*out_op_len`.
///
/// When `out_op_capacity` is too small, returns `CACHE_REUSE_ERR_INVALID`
/// with `*out_op_len` set to the capacity needed, so the caller can retry.
///
/// # Safety
///
/// Each pointer must be valid for its stated length and may be null only for
/// a zero length; `geometry` must be valid for one value and `out_op_len`
/// for one `u32`. Output buffers must not overlap any input.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cache_reuse_compile_archive_schedule(
    physical_len: u32,
    ranges: *const CacheReuseRange,
    range_len: u32,
    geometry: *const CacheReuseArchiveGeometry,
    out_ops: *mut CacheReuseArchiveOp,
    out_op_capacity: u32,
    out_op_len: *mut u32,
    error_out: *mut c_char,
    error_len: usize,
) -> u32 {
    run_status(error_out, error_len, "archive schedule", || {
        let written = unsafe { reset(out_op_len, "op length") }?;
        if geometry.is_null() {
            return Err("null geometry".into());
        }
        let g = unsafe { *geometry };
        let ranges = unsafe { borrow_ranges(ranges, range_len, "ranges") }?;
        let geometry = ArchiveGeometry {
            raw_arena_tokens: g.raw_arena_tokens,
            max_import_chunk: g.max_import_chunk,
            encoder_acquire_threshold: g.encoder_acquire_threshold,
            encoder_release_tail: g.encoder_release_tail,
            head_alignment: g.head_alignment,
        };
        let ops = cache_reuse_utils::compile_archive_schedule(physical_len, &ranges, geometry)
            .ok_or("invalid physical length, ranges or geometry")?;
        let needed = u32::try_from(ops.len()).map_err(|_| "schedule exceeds u32 ops")?;
        if needed > out_op_capacity {
            *written = needed;
            return Err(format!(
                "op output holds {out_op_capacity} ops; {needed} needed"
            ));
        }
        let out = unsafe { borrow_out(out_ops, out_op_capacity, "op output") }?;
        for (slot, &op) in out.iter_mut().zip(&ops) {
            *slot = abi_op(op);
        }
        *written = needed;
        Ok(())
    })
}
