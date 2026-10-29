//! Compile reused ranges into an ordered archive execution schedule.
//!
//! An engine rebuilding a physical sequence into a new archive imports each
//! reused range from the old archive and prefills (computes) every other
//! token, in ascending order, through a bounded raw arena that it flushes to
//! the archive. It computes a head at the aligned covered end, where the
//! archive is sealed, then finishes the short uncaptured tail and computes
//! the final head. [`compile_archive_schedule`] decides the order, chunking,
//! flushes and encoder residency; the engine only executes the ops.

use crate::placement::PlacedRange;
use crate::reconcile::ranges_ordered;

/// Engine limits that shape a schedule. Every field except
/// `encoder_acquire_threshold` and `encoder_release_tail` must be nonzero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveGeometry {
    /// Tokens the raw arena holds between flushes.
    pub raw_arena_tokens: u32,
    /// Most tokens one import chunk copies.
    pub max_import_chunk: u32,
    /// Smallest prefill segment worth holding encoder residency for; 0
    /// never acquires it.
    pub encoder_acquire_threshold: u32,
    /// Rows at the end of each prefill segment computed without encoder
    /// residency.
    pub encoder_release_tail: u32,
    /// The covered head is computed at the physical length rounded down to
    /// a multiple of this.
    pub head_alignment: u32,
}

/// One step of an archive schedule. Positions are physical destination
/// coordinates unless named `source_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveOp {
    /// Hold encoder residency across the following prefill chunks.
    AcquireEncoderResidency,
    /// Drop encoder residency.
    ReleaseEncoderResidency,
    /// Start reading the next range's source window,
    /// `[covered_start, range_end)` in source coordinates, while the gap
    /// before it is prefilled.
    PrefetchWindow { covered_start: u32, range_end: u32 },
    /// Materialize a reused range's source tokens for import.
    StageSpan {
        source_start: u32,
        destination_start: u32,
        count: u32,
    },
    /// Copy a staged chunk into the arena, then flush the arena to the new
    /// archive if `flush_arena`.
    ImportRebaseChunk {
        source_start: u32,
        destination_start: u32,
        count: u32,
        flush_arena: bool,
    },
    /// Compute `count` tokens from `start`. Before the covered head they are
    /// captured into the arena, flushed after the chunk if `flush_arena`.
    /// With `defer_decoder`, the next op continues this prefill directly, so
    /// the engine may defer decoder work into it.
    PrefillChunk {
        start: u32,
        count: u32,
        flush_arena: bool,
        defer_decoder: bool,
    },
    /// Compute the head at `covered_end` and seal the archive there.
    ComputeCoveredHead { covered_end: u32 },
    /// Compute the head at the end of the sequence.
    ComputeFinalHead { physical_len: u32 },
}

impl ArchiveGeometry {
    fn valid(&self) -> bool {
        self.raw_arena_tokens > 0 && self.max_import_chunk > 0 && self.head_alignment > 0
    }

    fn covered_end(&self, physical_len: u32) -> u32 {
        physical_len / self.head_alignment * self.head_alignment
    }
}

struct Builder {
    geometry: ArchiveGeometry,
    ops: Vec<ArchiveOp>,
    cursor: u32,
    arena: u32,
    resident: bool,
}

impl Builder {
    fn release(&mut self) {
        if self.resident {
            self.ops.push(ArchiveOp::ReleaseEncoderResidency);
            self.resident = false;
        }
    }

    /// Captured prefill of `[cursor, end)`, flushed at arena boundaries and
    /// at `end`.
    fn prefill_segment(&mut self, end: u32) {
        let Some(len) = end.checked_sub(self.cursor).filter(|&len| len > 0) else {
            return;
        };
        let g = self.geometry;
        self.acquire_for(len);
        let tail_start = end - g.encoder_release_tail.min(len);
        while self.cursor < end {
            let room = g.raw_arena_tokens - self.arena;
            let mut stop = end.min(self.cursor.saturating_add(room));
            if self.resident {
                stop = stop.min(tail_start);
                // The arena is empty at each segment start and every
                // resident chunk fills it, so a one-token resident chunk can
                // only be the row just before the tail: drop residency there.
                if stop - self.cursor == 1 {
                    self.release();
                    stop = end.min(self.cursor.saturating_add(room));
                }
            }
            let count = stop - self.cursor;
            let flush_arena = self.arena + count == g.raw_arena_tokens || stop == end;
            self.ops.push(ArchiveOp::PrefillChunk {
                start: self.cursor,
                count,
                flush_arena,
                defer_decoder: false,
            });
            self.cursor = stop;
            self.arena = if flush_arena { 0 } else { self.arena + count };
            if self.cursor == tail_start {
                self.release();
            }
        }
        self.release();
    }

    /// Acquire residency for a segment of `remaining` prefill rows when it is
    /// worth it and can cover multi-row chunks before the release tail.
    /// Segments start with an empty arena, so its first resident chunk holds
    /// `min(raw_arena_tokens, remaining - encoder_release_tail)` rows; one
    /// row would have to drop residency at once.
    fn acquire_for(&mut self, remaining: u32) {
        let g = self.geometry;
        if g.encoder_acquire_threshold > 0
            && remaining >= g.encoder_acquire_threshold
            && g.raw_arena_tokens > 1
            && remaining.saturating_sub(g.encoder_release_tail) > 1
        {
            self.ops.push(ArchiveOp::AcquireEncoderResidency);
            self.resident = true;
        }
    }

    fn import_range(&mut self, range: PlacedRange) {
        self.ops.push(ArchiveOp::StageSpan {
            source_start: range.source_start,
            destination_start: range.prepared_start,
            count: range.token_count,
        });
        let g = self.geometry;
        let mut copied = 0;
        while copied < range.token_count {
            let room = g.raw_arena_tokens - self.arena;
            let count = (range.token_count - copied)
                .min(g.max_import_chunk)
                .min(room);
            let flush_arena =
                self.arena + count == g.raw_arena_tokens || copied + count == range.token_count;
            self.ops.push(ArchiveOp::ImportRebaseChunk {
                source_start: range.source_start + copied,
                destination_start: range.prepared_start + copied,
                count,
                flush_arena,
            });
            copied += count;
            self.cursor += count;
            self.arena = if flush_arena { 0 } else { self.arena + count };
        }
    }
}

/// Compile the schedule that rebuilds a physical sequence of `physical_len`
/// tokens, importing `ranges` and prefilling everything else.
///
/// `ranges` must be nonempty, in strictly ascending, non-overlapping
/// destination order (`prepared_start`), and end at or before the covered
/// end (`physical_len` rounded down to `head_alignment`); clip them with
/// [`crate::clip_ranges_to_archive`] first. The result is deterministic and
/// passes [`verify_archive_schedule`].
///
/// Returns `None` for a zero `physical_len`, invalid geometry or ranges, or
/// a schedule that fails verification (a bug).
///
/// ```
/// # use cache_reuse_utils::*;
/// let geometry = ArchiveGeometry {
///     raw_arena_tokens: 8, max_import_chunk: 4, encoder_acquire_threshold: 0,
///     encoder_release_tail: 0, head_alignment: 4,
/// };
/// let range = PlacedRange { source_start: 0, destination_start: 4, token_count: 4, prepared_start: 4 };
/// let ops = compile_archive_schedule(10, &[range], geometry).unwrap();
/// assert_eq!(ops, [
///     ArchiveOp::PrefillChunk { start: 0, count: 4, flush_arena: true, defer_decoder: false },
///     ArchiveOp::StageSpan { source_start: 0, destination_start: 4, count: 4 },
///     ArchiveOp::ImportRebaseChunk { source_start: 0, destination_start: 4, count: 4, flush_arena: true },
///     ArchiveOp::ComputeCoveredHead { covered_end: 8 },
///     ArchiveOp::PrefillChunk { start: 8, count: 2, flush_arena: false, defer_decoder: false },
///     ArchiveOp::ComputeFinalHead { physical_len: 10 },
/// ]);
/// ```
pub fn compile_archive_schedule(
    physical_len: u32,
    ranges: &[PlacedRange],
    geometry: ArchiveGeometry,
) -> Option<Vec<ArchiveOp>> {
    if physical_len == 0 || !geometry.valid() {
        return None;
    }
    let covered_end = geometry.covered_end(physical_len);
    if !ranges_ordered(ranges, covered_end) {
        return None;
    }
    let mut builder = Builder {
        geometry,
        ops: Vec::with_capacity(estimated_ops(physical_len, ranges, geometry)),
        cursor: 0,
        arena: 0,
        resident: false,
    };
    for (index, &range) in ranges.iter().enumerate() {
        if index > 0 {
            builder.ops.push(ArchiveOp::PrefetchWindow {
                covered_start: range.source_start,
                range_end: range.source_end(),
            });
        }
        builder.prefill_segment(range.prepared_start);
        builder.import_range(range);
    }
    builder.prefill_segment(covered_end);
    if covered_end > 0 {
        builder
            .ops
            .push(ArchiveOp::ComputeCoveredHead { covered_end });
    }
    if physical_len > covered_end {
        builder.ops.push(ArchiveOp::PrefillChunk {
            start: covered_end,
            count: physical_len - covered_end,
            flush_arena: false,
            defer_decoder: false,
        });
    }
    builder
        .ops
        .push(ArchiveOp::ComputeFinalHead { physical_len });
    mark_deferred_decoders(&mut builder.ops);
    verify_archive_schedule(&builder.ops, physical_len, ranges, geometry).then_some(builder.ops)
}

/// Approximate op count: a few control ops per segment and range, imports
/// per import chunk, and prefill chunks per arena fill.
fn estimated_ops(physical_len: u32, ranges: &[PlacedRange], geometry: ArchiveGeometry) -> usize {
    let imported: u64 = ranges
        .iter()
        .map(|range| u64::from(range.token_count))
        .sum();
    let control = 6 * (ranges.len() + 1) + 4 * ranges.len() + 4;
    let chunks = imported / u64::from(geometry.max_import_chunk)
        + u64::from(physical_len) / u64::from(geometry.raw_arena_tokens);
    control.saturating_add(usize::try_from(chunks).unwrap_or(usize::MAX))
}

/// Set `defer_decoder` on each unflushed prefill chunk directly continued by
/// the next op.
fn mark_deferred_decoders(ops: &mut [ArchiveOp]) {
    for index in 0..ops.len().saturating_sub(1) {
        let next = ops[index + 1];
        if let ArchiveOp::PrefillChunk {
            start,
            count,
            flush_arena: false,
            defer_decoder,
        } = &mut ops[index]
        {
            *defer_decoder = continues(next, *start + *count);
        }
    }
}

fn continues(next: ArchiveOp, end: u32) -> bool {
    matches!(next, ArchiveOp::PrefillChunk { start, .. } if start == end)
}

/// Whether `ops` rebuilds `physical_len` tokens from `ranges` within
/// `geometry`:
///
/// - every token in `0..physical_len` is imported or prefilled exactly once,
///   in ascending order, and each range is staged, then imported whole,
///   in order;
/// - arena occupancy (captured tokens since the last flush) never exceeds
///   `raw_arena_tokens`, imports are at most `max_import_chunk`, and the
///   arena is empty at the covered head;
/// - encoder residency is never held for a one-token chunk, for the last
///   `encoder_release_tail` rows of a prefill segment, or across any op
///   other than prefill chunks;
/// - prefetches name the next range's source window, the covered head comes
///   once at the aligned covered end, the uncaptured tail never flushes, and
///   the schedule ends with the final head;
/// - `defer_decoder` is set only on an unflushed chunk directly continued
///   by the next op.
///
/// A prefill segment ends at the next range's destination start, or at the
/// covered end after the last range.
pub fn verify_archive_schedule(
    ops: &[ArchiveOp],
    physical_len: u32,
    ranges: &[PlacedRange],
    geometry: ArchiveGeometry,
) -> bool {
    if physical_len == 0 || !geometry.valid() {
        return false;
    }
    let covered_end = geometry.covered_end(physical_len);
    if !ranges_ordered(ranges, covered_end) {
        return false;
    }
    let mut state = Verifier {
        geometry,
        ranges,
        physical_len,
        covered_end,
        cursor: 0,
        arena: 0,
        resident: false,
        staged: None,
        next_range: 0,
        covered: false,
        finished: false,
    };
    ops.iter()
        .enumerate()
        .all(|(index, &op)| state.step(op, ops.get(index + 1).copied()))
        && state.finished
}

struct Verifier<'a> {
    geometry: ArchiveGeometry,
    ranges: &'a [PlacedRange],
    physical_len: u32,
    covered_end: u32,
    cursor: u32,
    arena: u32,
    resident: bool,
    /// The range being imported and how many of its tokens are done.
    staged: Option<(PlacedRange, u32)>,
    next_range: usize,
    covered: bool,
    finished: bool,
}

impl Verifier<'_> {
    /// Whether no import, residency or head is in progress.
    fn idle(&self) -> bool {
        !self.resident && self.staged.is_none() && !self.finished
    }

    fn segment_end(&self) -> u32 {
        self.ranges
            .get(self.next_range)
            .map_or(self.covered_end, |range| range.prepared_start)
    }

    /// Account `count` captured tokens; false if the arena would overflow.
    fn capture(&mut self, count: u32, flush_arena: bool) -> bool {
        let occupied = self.arena.saturating_add(count);
        self.arena = if flush_arena { 0 } else { occupied };
        occupied <= self.geometry.raw_arena_tokens
    }

    fn step(&mut self, op: ArchiveOp, next: Option<ArchiveOp>) -> bool {
        match op {
            ArchiveOp::AcquireEncoderResidency => {
                let ok = self.idle() && !self.covered;
                self.resident = true;
                ok
            }
            ArchiveOp::ReleaseEncoderResidency => std::mem::replace(&mut self.resident, false),
            ArchiveOp::PrefetchWindow {
                covered_start,
                range_end,
            } => {
                self.idle()
                    && self.ranges.get(self.next_range).is_some_and(|range| {
                        range.source_start == covered_start && range.source_end() == range_end
                    })
            }
            ArchiveOp::StageSpan {
                source_start,
                destination_start,
                count,
            } => {
                let Some(&range) = self.ranges.get(self.next_range) else {
                    return false;
                };
                let ok = self.idle()
                    && self.cursor == destination_start
                    && range.source_start == source_start
                    && range.prepared_start == destination_start
                    && range.token_count == count;
                self.staged = Some((range, 0));
                self.next_range += 1;
                ok
            }
            ArchiveOp::ImportRebaseChunk {
                source_start,
                destination_start,
                count,
                flush_arena,
            } => {
                let Some((range, done)) = self.staged else {
                    return false;
                };
                let ok = count > 0
                    && count <= self.geometry.max_import_chunk
                    && count <= range.token_count - done
                    && source_start == range.source_start + done
                    && destination_start == range.prepared_start + done
                    && destination_start == self.cursor
                    && self.capture(count, flush_arena);
                // Verification stops at the first failure, so only valid counts advance the state.
                if !ok {
                    return false;
                }
                self.cursor += count;
                self.staged = (done + count < range.token_count).then_some((range, done + count));
                true
            }
            ArchiveOp::PrefillChunk {
                start,
                count,
                flush_arena,
                defer_decoder,
            } => {
                let end = start.saturating_add(count);
                let shaped = self.staged.is_none()
                    && !self.finished
                    && count > 0
                    && start == self.cursor
                    && (!defer_decoder
                        || (!flush_arena && next.is_some_and(|op| continues(op, end))));
                // Tokens after the covered end (all of them when it is 0) are not captured.
                let placed = if self.covered || self.covered_end == 0 {
                    !self.resident && !flush_arena && end <= self.physical_len
                } else {
                    let segment_end = self.segment_end();
                    let residency_ok = !self.resident
                        || (count > 1
                            && end
                                <= segment_end.saturating_sub(self.geometry.encoder_release_tail));
                    end <= segment_end && residency_ok && self.capture(count, flush_arena)
                };
                self.cursor = end;
                shaped && placed
            }
            ArchiveOp::ComputeCoveredHead { covered_end } => {
                let ok = self.idle()
                    && !self.covered
                    && covered_end > 0
                    && covered_end == self.covered_end
                    && self.cursor == covered_end
                    && self.next_range == self.ranges.len()
                    && self.arena == 0;
                self.covered = true;
                ok
            }
            ArchiveOp::ComputeFinalHead { physical_len } => {
                let ok = self.idle()
                    && physical_len == self.physical_len
                    && self.cursor == physical_len
                    && self.covered == (self.covered_end > 0);
                self.finished = true;
                ok
            }
        }
    }
}
