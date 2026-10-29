//! C ABI bridge for the safe cache-reuse-utils core.
//!
//! This crate is the separate authorized C ABI boundary. It owns pointer
//! handling, input copying, panic containment and bounded result serialization;
//! the core crate remains std-only and unsafe-free.

use std::ffi::{c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::slice;
use std::time::{Duration, Instant};

use cache_reuse_utils::{
    AllowedInsertions, AnalysisTiming, CacheIdentity, CacheReuseAnalysis, CacheView,
    DEFAULT_PAD_PENALTY_TOKENS, LossAllowance, PlacementPolicy, PlacementStatus, PlanError,
    PlanErrorKind,
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
pub const CACHE_REUSE_MAX_RANGES: usize = 10;
/// Maximum insertion records emitted by the C ABI.
pub const CACHE_REUSE_MAX_INSERTIONS: usize = 10;
const _: () = assert!(CACHE_REUSE_MAX_RANGES == cache_reuse_utils::MAX_RANGES as usize);
const _: () = assert!(CACHE_REUSE_MAX_INSERTIONS == 10);

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
