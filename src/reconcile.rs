//! Fit planned ranges to an archive, and account for the ranges an engine
//! actually executed.
//!
//! Ranges are placed in a physical destination at `prepared_start` (the
//! prepared-target coordinates of [`PlacedRange`]) and read from a source
//! at `source_start`.

use crate::placement::PlacedRange;

/// Whether `ranges` are nonempty, overflow-free, and strictly ascending and
/// non-overlapping in destination (`prepared_start`) order, ending at or
/// before `destination_end`.
pub(crate) fn ranges_ordered(ranges: &[PlacedRange], destination_end: u32) -> bool {
    let mut cursor = 0u32;
    ranges.iter().all(|range| {
        let fits = range.token_count > 0
            && range.source_start.checked_add(range.token_count).is_some()
            && range
                .destination_start
                .checked_add(range.token_count)
                .is_some()
            && range.prepared_start >= cursor
            && range
                .prepared_start
                .checked_add(range.token_count)
                .is_some_and(|end| end <= destination_end);
        cursor = range.prepared_start.saturating_add(range.token_count);
        fits
    })
}

/// Clip `ranges` to what an archive can supply and write the survivors to
/// `out_clipped`, returning how many were written.
///
/// Each range keeps the part whose source lies in `[archive_start,
/// archive_end)` and whose destination ends at or before `target_cap`
/// rounded down to `alignment`. A range trimmed at the front moves all three
/// starts by the same multiple of `alignment`, and every count is rounded
/// down to a multiple of `alignment`; ranges left empty are dropped.
///
/// Returns `None` for a zero alignment, `archive_start > archive_end`,
/// ranges that are empty or out of destination order (see
/// [`PlacedRange::prepared_start`]), or an `out_clipped` shorter than the
/// survivors.
///
/// ```
/// # use cache_reuse_utils::{PlacedRange, clip_ranges_to_archive};
/// let range = PlacedRange { source_start: 0, destination_start: 4, token_count: 20, prepared_start: 4 };
/// let mut out = [range; 1];
/// // Destination capped at 18, rounded down to 16.
/// assert_eq!(clip_ranges_to_archive(&[range], 0, 100, 18, 4, &mut out), Some(1));
/// assert_eq!(out[0].token_count, 12);
/// ```
pub fn clip_ranges_to_archive(
    ranges: &[PlacedRange],
    archive_start: u32,
    archive_end: u32,
    target_cap: u32,
    alignment: u32,
    out_clipped: &mut [PlacedRange],
) -> Option<u32> {
    if alignment == 0 || archive_start > archive_end || !ranges_ordered(ranges, u32::MAX) {
        return None;
    }
    let ceiling = target_cap / alignment * alignment;
    let mut written = 0;
    for &range in ranges {
        let Some(clipped) = clip_one(range, archive_start, archive_end, ceiling, alignment) else {
            continue;
        };
        *out_clipped.get_mut(written)? = clipped;
        written += 1;
    }
    Some(written as u32)
}

fn clip_one(
    range: PlacedRange,
    archive_start: u32,
    archive_end: u32,
    ceiling: u32,
    alignment: u32,
) -> Option<PlacedRange> {
    // Trim the front by a multiple of the alignment so starts stay aligned.
    let shortfall = archive_start.saturating_sub(range.source_start);
    let shift = shortfall.div_ceil(alignment).checked_mul(alignment)?;
    if shift >= range.token_count {
        return None;
    }
    let source_start = range.source_start + shift;
    let prepared_start = range.prepared_start + shift;
    let mut count = range.token_count - shift;
    count = count.min(archive_end.saturating_sub(source_start));
    count = count.min(ceiling.saturating_sub(prepared_start));
    count = count / alignment * alignment;
    (count > 0).then_some(PlacedRange {
        source_start,
        destination_start: range.destination_start + shift,
        token_count: count,
        prepared_start,
    })
}

/// Token accounting for the ranges an engine executed, in physical
/// destination coordinates.
///
/// `first_reused_start + gap_tokens + reused_tokens + suffix_tokens` equals
/// the physical length, and `fresh_tokens` is everything not reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionSummary {
    /// Tokens copied by executed ranges.
    pub reused_tokens: u32,
    /// Fresh tokens between the first executed range and `covered_end`.
    pub gap_tokens: u32,
    /// Fresh tokens from `covered_end` to the physical length.
    pub suffix_tokens: u32,
    /// All tokens not reused: the prefix before the first range, the gaps
    /// and the suffix.
    pub fresh_tokens: u32,
    /// Destination start of the first executed range, or the physical length
    /// when none ran.
    pub first_reused_start: u32,
    /// Destination end of the last executed range, or the physical length
    /// when none ran.
    pub covered_end: u32,
}

/// Check that `executed` is a valid subset of `planned` and summarise it.
///
/// Each executed range must match a distinct planned range, in planned
/// order, with the same source and destination starts and a count no larger
/// than planned; an engine may stop a range early but not move it. Executed
/// counts must be nonzero multiples of `alignment`, and every range must end
/// within `physical_len`.
///
/// Returns `None` when any of that fails, for a zero alignment, or for
/// planned ranges that are empty or out of destination order.
///
/// ```
/// # use cache_reuse_utils::{PlacedRange, reconcile_executed_ranges};
/// let range = |start, count| PlacedRange { source_start: start, destination_start: start, token_count: count, prepared_start: start };
/// let planned = [range(4, 8), range(16, 8)];
/// let summary = reconcile_executed_ranges(&planned, &[range(4, 4), range(16, 8)], 32, 2).unwrap();
/// assert_eq!((summary.reused_tokens, summary.gap_tokens, summary.suffix_tokens), (12, 8, 8));
/// assert_eq!(summary.fresh_tokens, 20);
/// ```
pub fn reconcile_executed_ranges(
    planned: &[PlacedRange],
    executed: &[PlacedRange],
    physical_len: u32,
    alignment: u32,
) -> Option<ExecutionSummary> {
    if alignment == 0 || !ranges_ordered(planned, physical_len) {
        return None;
    }
    let mut next_planned = 0;
    let mut reused = 0u32;
    for range in executed {
        let offset = planned[next_planned..].iter().position(|plan| {
            plan.source_start == range.source_start
                && plan.prepared_start == range.prepared_start
                && plan.destination_start == range.destination_start
                && range.token_count <= plan.token_count
        })?;
        if range.token_count == 0 || range.token_count % alignment != 0 {
            return None;
        }
        next_planned += offset + 1;
        reused += range.token_count;
    }
    let (first_reused_start, covered_end) = match (executed.first(), executed.last()) {
        (Some(first), Some(last)) => (first.prepared_start, last.prepared_end()),
        _ => (physical_len, physical_len),
    };
    let gap_tokens = covered_end - first_reused_start - reused;
    let suffix_tokens = physical_len - covered_end;
    Some(ExecutionSummary {
        reused_tokens: reused,
        gap_tokens,
        suffix_tokens,
        fresh_tokens: physical_len - reused,
        first_reused_start,
        covered_end,
    })
}
