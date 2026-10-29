//! C ABI bridge for the safe cache-reuse-utils core.
//!
//! This crate is the separate authorized C ABI boundary. It owns pointer
//! handling, input copying, panic containment and bounded result serialization;
//! the core crate remains std-only and unsafe-free.
//!
//! Every exported function contains panics and retains no pointer after it
//! returns. Input and output buffers passed to one call must not overlap.

mod planning;

pub use planning::*;

use std::ffi::{c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::slice;
use std::time::{Duration, Instant};

use cache_reuse_utils::{
    AllowedInsertions, AnalysisTiming, CacheIdentity, CacheReuseAnalysis, CacheView,
    DEFAULT_PAD_PENALTY_TOKENS, DEFAULT_RANGE_PENALTY_TOKENS, Insertion, LossAllowance,
    PlacedRange, PlacementMaterializeError, PlacementPolicy, PlacementRecipe, PlacementStatus,
    PlanError, PlanErrorKind,
};

pub const CACHE_REUSE_COMPLETE: u32 = 0;
pub const CACHE_REUSE_NO_FEASIBLE: u32 = 1;
pub const CACHE_REUSE_DEADLINE_PARTIAL: u32 = 2;
pub const CACHE_REUSE_VARIANT_LIMIT: u32 = 3;
pub const CACHE_REUSE_STATE_LIMIT: u32 = 4;
pub const CACHE_REUSE_ERR_INVALID: u32 = 10;
pub const CACHE_REUSE_ERR_BUDGET: u32 = 11;
pub const CACHE_REUSE_ERR_CANCELLED: u32 = 12;
pub const CACHE_REUSE_ERR_ADMISSION: u32 = 13;
pub const CACHE_REUSE_ERR_PANIC: u32 = 14;
/// Maximum selected ranges emitted by the C ABI.
pub const CACHE_REUSE_MAX_RANGES: usize = 32;
/// Maximum insertion records emitted by the C ABI. A plan inserts at most
/// once before each range.
pub const CACHE_REUSE_MAX_INSERTIONS: usize = 32;
const _: () = assert!(CACHE_REUSE_MAX_RANGES == cache_reuse_utils::MAX_RANGES as usize);
const _: () = assert!(CACHE_REUSE_MAX_INSERTIONS == CACHE_REUSE_MAX_RANGES);

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct CacheReuseRange {
    pub source_start: u32,
    pub destination_start: u32,
    pub count: u32,
    pub prepared_start: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct CacheReuseInsertion {
    pub before_target_index: u32,
    /// First prepared-target index of this run: `before_target_index` plus
    /// all earlier insertion counts.
    pub prepared_start: u32,
    pub token: u32,
    pub count: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CacheReuseConfig {
    pub max_ranges: u32,
    pub minimum_match_tokens: u32,
    pub minimum_physical_reuse_tokens: u32,
    pub source_start_alignment: u32,
    pub prepared_target_start_alignment: u32,
    pub reuse_count_alignment: u32,
    pub minimum_prepared_reuse_start: u32,
    pub protected_prefix_tokens: u32,
    pub fresh_suffix_tokens: u32,
    pub max_trim_left: u32,
    pub max_trim_right: u32,
    pub pad_token: i32,
    pub loss_basis_points: u32,
    pub max_new_padding: u32,
    pub max_prepared_tokens: u32,
    pub variant_limit: u32,
    pub budget_ms: u32,
    pub allow_all_insertions: bool,
    pub allowed_insertions: *const u32,
    pub allowed_insertion_count: u32,
    /// Covered tokens one inserted pad must buy back in plan selection.
    /// 0 selects the default of 16 when padding is enabled.
    pub pad_penalty_tokens: u32,
    /// Covered tokens each range after the first must buy back in plan
    /// selection. 0 selects the default of 32; use 1 for the smallest
    /// penalty.
    pub range_penalty_tokens: u32,
}

#[repr(C)]
#[derive(Default)]
pub struct CacheReuseResult {
    pub status: u32,
    pub covered_tokens: u32,
    pub inserted_padding: u32,
    pub prepared_tokens: u32,
    pub best_useful: u32,
    pub admissible_min: u32,
    pub range_count: u32,
    pub insertion_count: u32,
    /// Candidate matches discovery handed to placement search, before
    /// placement. 0 when discovery did not run or found nothing.
    pub discovered_match_count: u32,
    /// Token length of the longest discovered match, or 0 without one.
    pub longest_match_tokens: u32,
    pub elapsed_ns: u64,
    pub preparation_ns: u64,
    pub discovery_ns: u64,
    pub search_ns: u64,
    pub discovery_entered: bool,
    pub search_entered: bool,
    pub timing_known: bool,
    pub ranges: [CacheReuseRange; CACHE_REUSE_MAX_RANGES],
    pub insertions: [CacheReuseInsertion; CACHE_REUSE_MAX_INSERTIONS],
}

fn set_error(buffer: *mut c_char, len: usize, message: &str) {
    if buffer.is_null() || len == 0 {
        return;
    }
    let bytes = message.as_bytes();
    let n = bytes.len().min(len - 1);
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), buffer.cast::<u8>(), n);
        *buffer.add(n) = 0;
    }
}

fn status_of_plan(status: PlacementStatus) -> u32 {
    match status {
        PlacementStatus::Complete => CACHE_REUSE_COMPLETE,
        PlacementStatus::NoFeasibleReuse => CACHE_REUSE_NO_FEASIBLE,
        PlacementStatus::DeadlinePartial => CACHE_REUSE_DEADLINE_PARTIAL,
        PlacementStatus::VariantLimitReached => CACHE_REUSE_VARIANT_LIMIT,
        PlacementStatus::StateLimitReached => CACHE_REUSE_STATE_LIMIT,
    }
}

enum BridgeError {
    Invalid(String),
    Plan(PlanError),
}

impl From<String> for BridgeError {
    fn from(value: String) -> Self {
        Self::Invalid(value)
    }
}
impl From<&str> for BridgeError {
    fn from(value: &str) -> Self {
        Self::Invalid(value.to_string())
    }
}

impl From<PlanError> for BridgeError {
    fn from(value: PlanError) -> Self {
        Self::Plan(value)
    }
}

fn error_status(error: &BridgeError) -> u32 {
    match error {
        BridgeError::Invalid(_) => CACHE_REUSE_ERR_INVALID,
        BridgeError::Plan(value) => error_status_plan(value),
    }
}

fn error_status_plan(error: &PlanError) -> u32 {
    match error.kind {
        PlanErrorKind::Cancelled => CACHE_REUSE_ERR_CANCELLED,
        PlanErrorKind::BudgetExhausted => CACHE_REUSE_ERR_BUDGET,
        PlanErrorKind::AdmissionDenied(_) | PlanErrorKind::PlacementIncomplete(_) => {
            CACHE_REUSE_ERR_ADMISSION
        }
        PlanErrorKind::InvalidInput(_) | PlanErrorKind::InvalidPlacementPolicy(_) => {
            CACHE_REUSE_ERR_INVALID
        }
        PlanErrorKind::Invariant(_) => CACHE_REUSE_ERR_PANIC,
    }
}

unsafe fn copy_tokens(pointer: *const i32, len: u32, what: &str) -> Result<Vec<u32>, String> {
    if len == 0 {
        return Ok(Vec::new());
    }
    if pointer.is_null() {
        return Err(format!("{what} pointer is null"));
    }
    let input = unsafe { slice::from_raw_parts(pointer, len as usize) };
    let mut output = Vec::new();
    output
        .try_reserve_exact(input.len())
        .map_err(|_| format!("{what} allocation refused"))?;
    for &token in input {
        if token < 0 {
            return Err(format!("{what} contains a negative token"));
        }
        output.push(token as u32);
    }
    Ok(output)
}

/// Copy the caller's insertion boundaries. Unused when all are allowed.
unsafe fn copy_allowed(config: &CacheReuseConfig) -> Result<Vec<u32>, String> {
    if config.allow_all_insertions || config.allowed_insertion_count == 0 {
        return Ok(Vec::new());
    }
    if config.allowed_insertions.is_null() {
        return Err("insertion pointer is null".into());
    }
    let input = unsafe {
        slice::from_raw_parts(
            config.allowed_insertions,
            config.allowed_insertion_count as usize,
        )
    };
    let mut output = Vec::new();
    output
        .try_reserve_exact(input.len())
        .map_err(|_| "insertion allocation refused")?;
    output.extend_from_slice(input);
    Ok(output)
}

fn apply_timing(result: &mut CacheReuseResult, timing: AnalysisTiming) {
    result.elapsed_ns = u64::try_from(timing.elapsed().as_nanos()).unwrap_or(u64::MAX);
    result.preparation_ns = u64::try_from(timing.preparation().as_nanos()).unwrap_or(u64::MAX);
    result.discovery_ns = timing
        .discovery()
        .map(|v| u64::try_from(v.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0);
    result.search_ns = timing
        .search()
        .map(|v| u64::try_from(v.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0);
    result.discovery_entered = timing.discovery().is_some();
    result.search_entered = timing.search().is_some();
    result.timing_known = true;
}

fn apply_result(result: &mut CacheReuseResult, plan: cache_reuse_utils::ReusePlan) -> bool {
    let ranges = plan.placement().ranges.as_slice();
    let insertions = plan.placement().insertions.as_slice();
    if ranges.len() > CACHE_REUSE_MAX_RANGES || insertions.len() > CACHE_REUSE_MAX_INSERTIONS {
        return false;
    }
    let metrics = plan.metrics();
    result.status = status_of_plan(plan.placement_status());
    result.covered_tokens = u32::try_from(plan.covered_tokens()).unwrap_or(u32::MAX);
    result.inserted_padding = metrics.inserted_padding;
    result.prepared_tokens = metrics.prepared_tokens;
    result.best_useful = u32::try_from(metrics.best_useful).unwrap_or(u32::MAX);
    result.admissible_min = u32::try_from(metrics.admissible_min).unwrap_or(u32::MAX);
    result.range_count = ranges.len() as u32;
    result.insertion_count = insertions.len() as u32;
    result.discovered_match_count = plan.discovered_match_count();
    result.longest_match_tokens = plan.longest_match_tokens();
    apply_timing(result, plan.timing());
    for (dst, src) in result.ranges.iter_mut().zip(ranges) {
        *dst = CacheReuseRange {
            source_start: src.source_start,
            destination_start: src.destination_start,
            count: src.token_count,
            prepared_start: src.prepared_start,
        };
    }
    for (dst, src) in result.insertions.iter_mut().zip(insertions) {
        *dst = CacheReuseInsertion {
            before_target_index: src.before_target_index,
            prepared_start: src.prepared_start,
            token: src.token,
            count: src.count,
        };
    }
    true
}

/// Run one planner call. Returns 0 when a plan (including an empty/partial plan)
/// was produced; nonzero means no plan and `result.status` is an error status.
///
/// # Safety
///
/// `target`/`source` must point to `target_len`/`source_len` valid `i32`
/// elements; pointers may be null only for zero lengths. `config` and `result`
/// must be valid aligned pointers for the call. The cancel callback, when
/// present, must be callable with `cancel_ud` and must not unwind. No input or
/// output pointer is retained after return.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cache_reuse_plan(
    target: *const i32,
    target_len: u32,
    source: *const i32,
    source_len: u32,
    cache_identity: u64,
    config: *const CacheReuseConfig,
    result: *mut CacheReuseResult,
    error: *mut c_char,
    error_len: usize,
    cancel: Option<unsafe extern "C" fn(*mut c_void) -> bool>,
    cancel_ud: *mut c_void,
) -> i32 {
    if result.is_null() {
        set_error(error, error_len, "null planner result");
        return 1;
    }
    let result_ref = unsafe { &mut *result };
    *result_ref = CacheReuseResult {
        status: CACHE_REUSE_ERR_INVALID,
        ..Default::default()
    };
    if config.is_null() {
        set_error(error, error_len, "null planner config");
        return 1;
    }
    let config = unsafe { &*config };
    let deadline = match Instant::now().checked_add(Duration::from_millis(config.budget_ms as u64))
    {
        Some(value) => value,
        None => {
            set_error(error, error_len, "planner deadline overflow");
            return 1;
        }
    };
    let outcome = catch_unwind(AssertUnwindSafe(
        || -> Result<cache_reuse_utils::ReusePlan, BridgeError> {
            let target = unsafe { copy_tokens(target, target_len, "target") }?;
            let source = unsafe { copy_tokens(source, source_len, "source") }?;
            if target.len() > u32::MAX as usize || source.len() > u32::MAX as usize {
                return Err("input length exceeds u32".into());
            }
            let allowed = unsafe { copy_allowed(config) }?;
            if config.max_ranges == 0 || config.max_ranges > CACHE_REUSE_MAX_RANGES as u32 {
                return Err("invalid max_ranges".into());
            }
            let pad_token = if config.pad_token < 0 {
                None
            } else {
                Some(u32::try_from(config.pad_token).map_err(|_| "invalid pad token")?)
            };
            let mut cancel_closure =
                || cancel.is_some_and(|callback| unsafe { callback(cancel_ud) });
            let analysis = CacheReuseAnalysis {
                context_tokens: &target,
                cache: CacheView {
                    tokens: &source,
                    identity: CacheIdentity::new(cache_identity),
                    minimum_match_tokens: config.minimum_match_tokens,
                },
                k: config.max_ranges,
                deadline,
                cancel: &mut cancel_closure,
                placement: PlacementPolicy {
                    loss_allowance: LossAllowance::from_basis_points(
                        u16::try_from(config.loss_basis_points).map_err(|_| "invalid loss")?,
                    ),
                    protected_prefix_tokens: config.protected_prefix_tokens,
                    fresh_suffix_tokens: config.fresh_suffix_tokens,
                    allowed_insertions: if config.allow_all_insertions {
                        AllowedInsertions::All
                    } else {
                        AllowedInsertions::Boundaries(&allowed)
                    },
                    pad_token,
                    pad_penalty_tokens: match config.pad_penalty_tokens {
                        0 if pad_token.is_some() && config.max_new_padding > 0 => {
                            DEFAULT_PAD_PENALTY_TOKENS
                        }
                        value => value,
                    },
                    range_penalty_tokens: match config.range_penalty_tokens {
                        0 => DEFAULT_RANGE_PENALTY_TOKENS,
                        value => value,
                    },
                    source_start_alignment: config.source_start_alignment,
                    prepared_target_start_alignment: config.prepared_target_start_alignment,
                    reuse_count_alignment: config.reuse_count_alignment,
                    minimum_prepared_reuse_start: config.minimum_prepared_reuse_start,
                    minimum_physical_reuse_tokens: config.minimum_physical_reuse_tokens,
                    max_prepared_tokens: config.max_prepared_tokens as usize,
                    max_new_padding: config.max_new_padding,
                    max_trim_left: config.max_trim_left,
                    max_trim_right: config.max_trim_right,
                    max_variants: if config.variant_limit == 0 {
                        cache_reuse_utils::MAX_PLACEMENT_VARIANTS
                    } else {
                        config.variant_limit as usize
                    },
                },
            };
            analysis.analyze().map_err(BridgeError::Plan)
        },
    ));
    match outcome {
        Ok(Ok(plan)) => {
            if apply_result(result_ref, plan) {
                0
            } else {
                result_ref.status = CACHE_REUSE_ERR_INVALID;
                set_error(error, error_len, "planner result exceeds ABI bounds");
                1
            }
        }
        Ok(Err(plan_error)) => {
            result_ref.status = error_status(&plan_error);
            if let BridgeError::Plan(value) = &plan_error {
                apply_timing(result_ref, value.timing);
            }
            let message = match &plan_error {
                BridgeError::Invalid(value) => value.clone(),
                BridgeError::Plan(value) => value.to_string(),
            };
            set_error(error, error_len, &message);
            1
        }
        Err(_) => {
            result_ref.status = CACHE_REUSE_ERR_PANIC;
            set_error(error, error_len, "planner panicked");
            1
        }
    }
}

/// Result of `cache_reuse_common_prefix`. Field meanings match the core
/// `CommonPrefix`.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct CacheReuseCommonPrefix {
    /// Leading logical tokens of the physical sequence equal to the start of
    /// the logical sequence.
    pub common_logical_tokens: u32,
    /// Physical tokens up to and including the last common logical token.
    pub physical_prefix_tokens: u32,
    /// Pads interleaved inside that physical prefix.
    pub carried_pads: u32,
    /// Logical (non-pad) tokens in the whole physical sequence.
    pub live_logical_tokens: u32,
}

/// Run a status-returning bridge call with panic containment.
fn run_status(
    error: *mut c_char,
    error_len: usize,
    name: &str,
    call: impl FnOnce() -> Result<(), String>,
) -> u32 {
    match catch_unwind(AssertUnwindSafe(call)) {
        Ok(Ok(())) => CACHE_REUSE_COMPLETE,
        Ok(Err(message)) => {
            set_error(error, error_len, &message);
            CACHE_REUSE_ERR_INVALID
        }
        Err(_) => {
            set_error(error, error_len, &format!("{name} panicked"));
            CACHE_REUSE_ERR_PANIC
        }
    }
}

/// Borrow `len` caller tokens as `u32` without copying. With
/// `require_non_negative`, a negative token is rejected; otherwise token
/// values are opaque and only compared or copied bit for bit.
unsafe fn borrow_tokens<'a>(
    pointer: *const i32,
    len: u32,
    what: &str,
    require_non_negative: bool,
) -> Result<&'a [u32], String> {
    if len == 0 {
        return Ok(&[]);
    }
    if pointer.is_null() {
        return Err(format!("{what} pointer is null"));
    }
    // i32 and u32 share size and alignment; the bits are reinterpreted.
    let tokens = unsafe { slice::from_raw_parts(pointer.cast::<u32>(), len as usize) };
    if require_non_negative && tokens.iter().any(|&token| token > i32::MAX as u32) {
        return Err(format!("{what} contains a negative token"));
    }
    Ok(tokens)
}

/// Borrow `len` caller `u32` values without copying.
unsafe fn borrow_u32<'a>(pointer: *const u32, len: u32, what: &str) -> Result<&'a [u32], String> {
    if len == 0 {
        return Ok(&[]);
    }
    if pointer.is_null() {
        return Err(format!("{what} pointer is null"));
    }
    Ok(unsafe { slice::from_raw_parts(pointer, len as usize) })
}

/// Borrow a caller output buffer of `len` elements.
unsafe fn borrow_out<'a, T>(pointer: *mut T, len: u32, what: &str) -> Result<&'a mut [T], String> {
    if len == 0 {
        return Ok(&mut []);
    }
    if pointer.is_null() {
        return Err(format!("{what} pointer is null"));
    }
    Ok(unsafe { slice::from_raw_parts_mut(pointer, len as usize) })
}

/// Rebuild the core recipe from a planner result for `target_len` tokens.
fn recipe_of(result: &CacheReuseResult, target_len: u32) -> Result<PlacementRecipe, String> {
    if result.status > CACHE_REUSE_STATE_LIMIT {
        return Err("result does not hold a plan".into());
    }
    let range_count = result.range_count as usize;
    let insertion_count = result.insertion_count as usize;
    if range_count > CACHE_REUSE_MAX_RANGES || insertion_count > CACHE_REUSE_MAX_INSERTIONS {
        return Err("result counts exceed ABI bounds".into());
    }
    if result.prepared_tokens.checked_sub(result.inserted_padding) != Some(target_len) {
        return Err("target length does not match the result".into());
    }
    let insertions = &result.insertions[..insertion_count];
    if insertions
        .iter()
        .any(|insertion| insertion.token > i32::MAX as u32)
    {
        return Err("result pad token is not a valid i32 token".into());
    }
    Ok(PlacementRecipe {
        // Identity is correlation metadata and plays no part here.
        cache_identity: CacheIdentity::new(0),
        original_tokens: target_len,
        prepared_tokens: result.prepared_tokens,
        ranges: result.ranges[..range_count]
            .iter()
            .map(|range| PlacedRange {
                source_start: range.source_start,
                destination_start: range.destination_start,
                token_count: range.count,
                prepared_start: range.prepared_start,
            })
            .collect(),
        insertions: insertions
            .iter()
            .map(|insertion| Insertion {
                before_target_index: insertion.before_target_index,
                prepared_start: insertion.prepared_start,
                token: insertion.token,
                count: insertion.count,
            })
            .collect(),
    })
}

fn materialize_message(error: PlacementMaterializeError) -> String {
    match error {
        PlacementMaterializeError::InputShape => "output length does not match the result".into(),
        PlacementMaterializeError::SourceRange => "result range exceeds the source".into(),
        PlacementMaterializeError::ContentMismatch { index } => {
            format!("source token differs from target token {index}")
        }
        PlacementMaterializeError::RecipeShape => "result recipe is inconsistent".into(),
        PlacementMaterializeError::Allocation => "allocation refused".into(),
    }
}

/// Write the prepared target for a planner `result` into caller buffers.
///
/// `out_prepared_len` must equal `result->prepared_tokens` and `target_len`
/// the target length that was planned. When `out_pad_positions` is non-null
/// or `out_pad_len` is nonzero, `out_pad_len` must equal
/// `result->inserted_padding` and receives each pad's prepared index in
/// increasing order. Every reused source token must equal the target token
/// it replaces. Returns `CACHE_REUSE_COMPLETE`, `CACHE_REUSE_ERR_INVALID` or
/// `CACHE_REUSE_ERR_PANIC`; on error the outputs may be partially written.
/// Token buffers are borrowed, not copied.
///
/// # Safety
///
/// Each pointer must be valid for its stated length (`result` for one
/// value) and may be null only for a zero length. Output buffers must not
/// overlap any input.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cache_reuse_materialize(
    target_tokens: *const i32,
    target_len: u32,
    source_tokens: *const i32,
    source_len: u32,
    result: *const CacheReuseResult,
    out_prepared_tokens: *mut i32,
    out_prepared_len: u32,
    out_pad_positions: *mut u32,
    out_pad_len: u32,
    error_out: *mut c_char,
    error_len: usize,
) -> u32 {
    run_status(error_out, error_len, "materialize", || {
        if result.is_null() {
            return Err("null planner result".into());
        }
        let target = unsafe { borrow_tokens(target_tokens, target_len, "target", true) }?;
        let source = unsafe { borrow_tokens(source_tokens, source_len, "source", true) }?;
        let recipe = recipe_of(unsafe { &*result }, target_len)?;
        let prepared = unsafe {
            borrow_out(
                out_prepared_tokens.cast::<u32>(),
                out_prepared_len,
                "prepared output",
            )
        }?;
        let pads = if out_pad_positions.is_null() && out_pad_len == 0 {
            None
        } else {
            Some(unsafe { borrow_out(out_pad_positions, out_pad_len, "pad output") }?)
        };
        recipe
            .materialize_into(target, source, prepared, pads)
            .map_err(materialize_message)
    })
}

/// Copy the logical (non-pad) tokens of a physical sequence.
///
/// `pad_positions` must be strictly increasing physical indices below
/// `physical_len`. `out_logical_capacity` must be at least
/// `physical_len - pad_len`; the count written is stored in
/// `*out_logical_len`, which is set to 0 on error. Token values are opaque.
///
/// # Safety
///
/// Each pointer must be valid for its stated length and may be null only for
/// a zero length; `out_logical_len` must be valid for one `u32`. Output
/// buffers must not overlap any input.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cache_reuse_project_logical(
    physical_tokens: *const i32,
    physical_len: u32,
    pad_positions: *const u32,
    pad_len: u32,
    out_logical_tokens: *mut i32,
    out_logical_capacity: u32,
    out_logical_len: *mut u32,
    error_out: *mut c_char,
    error_len: usize,
) -> u32 {
    run_status(error_out, error_len, "projection", || {
        if out_logical_len.is_null() {
            return Err("null logical length output".into());
        }
        unsafe { *out_logical_len = 0 };
        let physical = unsafe { borrow_tokens(physical_tokens, physical_len, "physical", false) }?;
        let pads = unsafe { borrow_u32(pad_positions, pad_len, "pad positions") }?;
        let out = unsafe {
            borrow_out(
                out_logical_tokens.cast::<u32>(),
                out_logical_capacity,
                "logical output",
            )
        }?;
        let written = cache_reuse_utils::project_to_logical(physical, pads, out)
            .ok_or("invalid pad positions or logical output too small")?;
        unsafe { *out_logical_len = written };
        Ok(())
    })
}

/// Store in `*out_logical_boundary` the number of logical tokens before
/// `physical_boundary`. Set to 0 on error.
///
/// # Safety
///
/// `pad_positions` must be valid for `pad_len` values and may be null only
/// for a zero length; `out_logical_boundary` must be valid for one `u32`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cache_reuse_logical_boundary(
    physical_boundary: u32,
    physical_len: u32,
    pad_positions: *const u32,
    pad_len: u32,
    out_logical_boundary: *mut u32,
    error_out: *mut c_char,
    error_len: usize,
) -> u32 {
    run_status(error_out, error_len, "boundary mapping", || {
        if out_logical_boundary.is_null() {
            return Err("null logical boundary output".into());
        }
        unsafe { *out_logical_boundary = 0 };
        let pads = unsafe { borrow_u32(pad_positions, pad_len, "pad positions") }?;
        let boundary = cache_reuse_utils::logical_boundary(physical_boundary, physical_len, pads)
            .ok_or("boundary beyond physical length or invalid pad positions")?;
        unsafe { *out_logical_boundary = boundary };
        Ok(())
    })
}

/// Measure how much of `logical_tokens` the physical sequence already holds
/// as a pad-skipping prefix. `*out` is zeroed on error. Token values are
/// opaque.
///
/// # Safety
///
/// Each pointer must be valid for its stated length and may be null only for
/// a zero length; `out` must be valid for one `CacheReuseCommonPrefix`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cache_reuse_common_prefix(
    logical_tokens: *const i32,
    logical_len: u32,
    physical_tokens: *const i32,
    physical_len: u32,
    pad_positions: *const u32,
    pad_len: u32,
    out: *mut CacheReuseCommonPrefix,
    error_out: *mut c_char,
    error_len: usize,
) -> u32 {
    run_status(error_out, error_len, "common prefix", || {
        if out.is_null() {
            return Err("null common prefix output".into());
        }
        unsafe { *out = CacheReuseCommonPrefix::default() };
        let logical = unsafe { borrow_tokens(logical_tokens, logical_len, "logical", false) }?;
        let physical = unsafe { borrow_tokens(physical_tokens, physical_len, "physical", false) }?;
        let pads = unsafe { borrow_u32(pad_positions, pad_len, "pad positions") }?;
        let prefix = cache_reuse_utils::common_logical_prefix(logical, physical, pads)
            .ok_or("invalid pad positions")?;
        unsafe {
            *out = CacheReuseCommonPrefix {
                common_logical_tokens: prefix.common_logical_tokens,
                physical_prefix_tokens: prefix.physical_prefix_tokens,
                carried_pads: prefix.carried_pads,
                live_logical_tokens: prefix.live_logical_tokens,
            }
        };
        Ok(())
    })
}
