//! Archive schedule compilation and verification.

use cache_reuse_utils::{
    ArchiveGeometry, ArchiveOp, PlacedRange, compile_archive_schedule, verify_archive_schedule,
};

use ArchiveOp::{
    AcquireEncoderResidency as Acquire, ComputeCoveredHead, ComputeFinalHead, ImportRebaseChunk,
    PrefetchWindow, PrefillChunk, ReleaseEncoderResidency as Release, StageSpan,
};

const GEOMETRY: ArchiveGeometry = ArchiveGeometry {
    raw_arena_tokens: 6,
    max_import_chunk: 4,
    encoder_acquire_threshold: 5,
    encoder_release_tail: 2,
    head_alignment: 4,
};

fn range(source_start: u32, prepared_start: u32, token_count: u32) -> PlacedRange {
    PlacedRange {
        source_start,
        destination_start: prepared_start,
        token_count,
        prepared_start,
    }
}

fn prefill(start: u32, count: u32, flush_arena: bool) -> ArchiveOp {
    PrefillChunk {
        start,
        count,
        flush_arena,
        defer_decoder: false,
    }
}

fn import(source_start: u32, destination_start: u32, count: u32, flush_arena: bool) -> ArchiveOp {
    ImportRebaseChunk {
        source_start,
        destination_start,
        count,
        flush_arena,
    }
}

const RANGES: [PlacedRange; 1] = [PlacedRange {
    source_start: 100,
    destination_start: 7,
    token_count: 5,
    prepared_start: 7,
}];

fn golden() -> Vec<ArchiveOp> {
    vec![
        // Prefix [0, 7): resident until its last 2 rows, flushing when the 6-token arena fills.
        Acquire,
        prefill(0, 5, false),
        Release,
        prefill(5, 1, true),
        prefill(6, 1, true),
        // The range, in import chunks of at most 4, flushed when it ends.
        StageSpan {
            source_start: 100,
            destination_start: 7,
            count: 5,
        },
        import(100, 7, 4, false),
        import(104, 11, 1, true),
        // Suffix [12, 20) up to the covered end, 22 rounded down to 4.
        Acquire,
        prefill(12, 6, true),
        Release,
        prefill(18, 2, true),
        ComputeCoveredHead { covered_end: 20 },
        // The uncaptured tail.
        prefill(20, 2, false),
        ComputeFinalHead { physical_len: 22 },
    ]
}

#[test]
fn compiles_the_expected_schedule() {
    let ops = compile_archive_schedule(22, &RANGES, GEOMETRY).unwrap();
    assert_eq!(ops, golden());
    assert!(verify_archive_schedule(&ops, 22, &RANGES, GEOMETRY));
    // Deterministic.
    assert_eq!(compile_archive_schedule(22, &RANGES, GEOMETRY), Some(ops));
}

#[test]
fn prefetches_the_next_range_before_each_gap() {
    let ranges = [range(50, 0, 4), range(10, 8, 4), range(90, 16, 4)];
    let geometry = ArchiveGeometry {
        encoder_acquire_threshold: 0,
        ..GEOMETRY
    };
    let ops = compile_archive_schedule(24, &ranges, geometry).unwrap();
    let prefetches: Vec<_> = ops
        .iter()
        .filter(|op| matches!(op, PrefetchWindow { .. }))
        .collect();
    assert_eq!(
        prefetches,
        [
            &PrefetchWindow {
                covered_start: 10,
                range_end: 14
            },
            &PrefetchWindow {
                covered_start: 90,
                range_end: 94
            },
        ]
    );
    // Each prefetch directly follows the previous range's last import.
    for (index, op) in ops.iter().enumerate() {
        if matches!(op, PrefetchWindow { .. }) {
            assert!(matches!(
                ops[index - 1],
                ImportRebaseChunk {
                    flush_arena: true,
                    ..
                }
            ));
        }
    }
    assert!(!ops.contains(&Acquire));
}

#[test]
fn short_sequences_have_no_covered_head() {
    let ops = compile_archive_schedule(3, &[], GEOMETRY).unwrap();
    assert_eq!(
        ops,
        [prefill(0, 3, false), ComputeFinalHead { physical_len: 3 }]
    );
    let ops = compile_archive_schedule(8, &[], GEOMETRY).unwrap();
    assert_eq!(
        ops,
        [
            Acquire,
            prefill(0, 6, true),
            Release,
            prefill(6, 2, true),
            ComputeCoveredHead { covered_end: 8 },
            ComputeFinalHead { physical_len: 8 },
        ]
    );
}

#[test]
fn residency_is_not_acquired_when_it_cannot_cover_a_multi_row_chunk() {
    // A one-token arena makes every chunk a single row.
    let tiny_arena = ArchiveGeometry {
        raw_arena_tokens: 1,
        ..GEOMETRY
    };
    let ops = compile_archive_schedule(12, &[], tiny_arena).unwrap();
    assert!(!ops.contains(&Acquire));
    // Eight rows less a 7-row tail leave one resident row: not worth it.
    let long_tail = ArchiveGeometry {
        encoder_release_tail: 7,
        ..GEOMETRY
    };
    assert!(
        !compile_archive_schedule(8, &[], long_tail)
            .unwrap()
            .contains(&Acquire)
    );
    let shorter_tail = ArchiveGeometry {
        encoder_release_tail: 6,
        ..GEOMETRY
    };
    assert!(
        compile_archive_schedule(8, &[], shorter_tail)
            .unwrap()
            .contains(&Acquire)
    );
}

#[test]
fn rejects_invalid_requests() {
    assert_eq!(compile_archive_schedule(0, &[], GEOMETRY), None);
    for geometry in [
        ArchiveGeometry {
            raw_arena_tokens: 0,
            ..GEOMETRY
        },
        ArchiveGeometry {
            max_import_chunk: 0,
            ..GEOMETRY
        },
        ArchiveGeometry {
            head_alignment: 0,
            ..GEOMETRY
        },
    ] {
        assert_eq!(compile_archive_schedule(22, &RANGES, geometry), None);
    }
    // A range past the covered end, overlapping, out of order, or empty.
    assert_eq!(
        compile_archive_schedule(22, &[range(0, 18, 4)], GEOMETRY),
        None
    );
    assert_eq!(
        compile_archive_schedule(22, &[range(0, 0, 8), range(0, 6, 4)], GEOMETRY),
        None
    );
    assert_eq!(
        compile_archive_schedule(22, &[range(0, 8, 4), range(0, 0, 4)], GEOMETRY),
        None
    );
    assert_eq!(
        compile_archive_schedule(22, &[range(0, 4, 0)], GEOMETRY),
        None
    );
}

#[test]
fn verification_rejects_each_broken_invariant() {
    let rejects = |edit: &dyn Fn(&mut Vec<ArchiveOp>)| {
        let mut ops = golden();
        edit(&mut ops);
        !verify_archive_schedule(&ops, 22, &RANGES, GEOMETRY)
    };
    // Coverage: a missing, repeated or reordered chunk.
    assert!(rejects(&|ops| {
        ops.remove(4);
    }));
    assert!(rejects(&|ops| ops.insert(4, ops[3])));
    assert!(rejects(&|ops| ops.swap(3, 4)));
    // Arena: no flush where it fills, so the next chunk overflows it.
    assert!(rejects(&|ops| ops[9] = prefill(12, 6, false)));
    // The arena must be empty at the covered head.
    assert!(rejects(&|ops| ops[11] = prefill(18, 2, false)));
    // Imports: larger than the import chunk, or not matching the stage.
    assert!(rejects(&|ops| {
        ops.splice(6..8, [import(100, 7, 5, true)]);
    }));
    assert!(rejects(&|ops| ops[6] = import(101, 7, 4, false)));
    // Residency held into the release tail, or across a one-token chunk.
    assert!(rejects(&|ops| {
        ops.remove(10);
        ops.insert(12, Release);
    }));
    assert!(rejects(&|ops| {
        ops.remove(2);
        ops.insert(4, Release);
    }));
    // Residency held across the stage, acquired twice or released twice.
    assert!(rejects(&|ops| {
        ops.insert(5, Acquire);
        ops.insert(7, Release);
    }));
    assert!(rejects(&|ops| ops.insert(1, Acquire)));
    assert!(rejects(&|ops| ops.insert(1, Release)));
    // Heads: missing, misplaced, or followed by more work.
    assert!(rejects(&|ops| {
        ops.remove(12);
    }));
    assert!(rejects(
        &|ops| ops[12] = ComputeCoveredHead { covered_end: 16 }
    ));
    assert!(rejects(&|ops| {
        ops.pop();
    }));
    assert!(rejects(
        &|ops| ops.push(ComputeFinalHead { physical_len: 22 })
    ));
    // The uncaptured tail cannot flush.
    assert!(rejects(&|ops| ops[13] = prefill(20, 2, true)));
    // A prefetch must name the next range's whole source window.
    assert!(rejects(&|ops| ops.insert(
        5,
        PrefetchWindow {
            covered_start: 100,
            range_end: 104
        }
    )));
    assert!(!rejects(&|ops| ops.insert(
        5,
        PrefetchWindow {
            covered_start: 100,
            range_end: 105
        }
    )));
    // Deferred decoding needs a directly continuing prefill.
    assert!(rejects(&|ops| {
        ops[1] = PrefillChunk {
            start: 0,
            count: 5,
            flush_arena: false,
            defer_decoder: true,
        }
    }));
    // Hostile values are rejected without overflowing.
    assert!(rejects(&|ops| ops[6] = import(100, 7, u32::MAX, false)));
    assert!(rejects(&|ops| ops[1] = prefill(0, u32::MAX, false)));
    let huge = ArchiveGeometry {
        raw_arena_tokens: u32::MAX,
        max_import_chunk: u32::MAX,
        ..GEOMETRY
    };
    assert!(!verify_archive_schedule(
        &[import(0, 0, u32::MAX, false)],
        22,
        &RANGES,
        huge
    ));
    // The golden schedule itself passes.
    assert!(!rejects(&|_| {}));
}

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u32) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) % n as u64) as u32
    }
}

/// An independent replay of the schedule invariants.
fn check_schedule(
    ops: &[ArchiveOp],
    physical_len: u32,
    ranges: &[PlacedRange],
    g: ArchiveGeometry,
) {
    let covered_end = physical_len / g.head_alignment * g.head_alignment;
    let segment_end = |at: u32| {
        ranges
            .iter()
            .map(|range| range.prepared_start)
            .find(|&start| start >= at)
            .unwrap_or(covered_end)
    };
    let mut cursor = 0;
    let mut arena = 0;
    let mut resident = false;
    let mut imported = vec![0u32; ranges.len()];
    let mut staged = Vec::new();
    let mut covered = false;
    for (index, &op) in ops.iter().enumerate() {
        match op {
            Acquire => {
                assert!(!resident);
                resident = true;
            }
            Release => {
                assert!(resident);
                // Residency is held until the release tail (or the single
                // row before it), never dropped early inside a segment.
                let tail_start = segment_end(cursor).saturating_sub(g.encoder_release_tail);
                assert!(
                    cursor + 1 >= tail_start,
                    "early release at {cursor}, tail at {tail_start}"
                );
                resident = false;
            }
            PrefetchWindow {
                covered_start,
                range_end,
            } => {
                let next = ranges[staged.len()];
                assert_eq!(
                    (covered_start, range_end),
                    (next.source_start, next.source_end())
                );
            }
            StageSpan {
                source_start,
                destination_start,
                count,
            } => {
                assert!(!resident);
                let next = ranges[staged.len()];
                assert_eq!(
                    (source_start, destination_start, count),
                    (next.source_start, next.prepared_start, next.token_count)
                );
                staged.push(index);
            }
            ImportRebaseChunk {
                source_start,
                destination_start,
                count,
                flush_arena,
            } => {
                assert!(!resident && count <= g.max_import_chunk);
                let current = staged.len() - 1;
                let range = ranges[current];
                assert_eq!(destination_start, cursor);
                assert_eq!(
                    source_start - range.source_start,
                    destination_start - range.prepared_start
                );
                imported[current] += count;
                arena += count;
                assert!(arena <= g.raw_arena_tokens);
                if flush_arena {
                    arena = 0;
                }
                cursor += count;
            }
            PrefillChunk {
                start,
                count,
                flush_arena,
                ..
            } => {
                assert_eq!(start, cursor);
                assert!(count > 0);
                if resident {
                    assert!(count > 1);
                    assert!(start + count + g.encoder_release_tail <= segment_end(start));
                }
                if start < covered_end {
                    assert!(start + count <= segment_end(start));
                    arena += count;
                    assert!(arena <= g.raw_arena_tokens);
                    if flush_arena {
                        arena = 0;
                    }
                } else {
                    assert!(!flush_arena);
                }
                cursor += count;
            }
            ComputeCoveredHead { covered_end: end } => {
                assert!(!resident && !covered);
                assert_eq!((end, cursor, arena), (covered_end, covered_end, 0));
                covered = true;
            }
            ComputeFinalHead { physical_len: len } => {
                assert_eq!(index, ops.len() - 1);
                assert!(!resident);
                assert_eq!((len, cursor), (physical_len, physical_len));
            }
        }
    }
    assert_eq!(covered, covered_end > 0);
    let counts: Vec<_> = ranges.iter().map(|range| range.token_count).collect();
    assert_eq!(imported, counts);
}

#[test]
fn random_schedules_satisfy_an_independent_replay() {
    let mut rng = Rng(0x5eed);
    for _ in 0..3000 {
        let geometry = ArchiveGeometry {
            raw_arena_tokens: 1 + rng.below(40),
            max_import_chunk: 1 + rng.below(20),
            encoder_acquire_threshold: rng.below(30),
            encoder_release_tail: rng.below(12),
            head_alignment: 1 + rng.below(8),
        };
        let physical_len = 1 + rng.below(300);
        let covered_end = physical_len / geometry.head_alignment * geometry.head_alignment;
        let mut ranges = Vec::new();
        let mut at = rng.below(10);
        while ranges.len() < 6 && at < covered_end {
            let count = 1 + rng.below((covered_end - at).min(40));
            ranges.push(range(rng.below(1000), at, count));
            at += count + rng.below(30);
        }
        let ops = compile_archive_schedule(physical_len, &ranges, geometry)
            .unwrap_or_else(|| panic!("{physical_len} {ranges:?} {geometry:?}"));
        check_schedule(&ops, physical_len, &ranges, geometry);
        assert!(verify_archive_schedule(
            &ops,
            physical_len,
            &ranges,
            geometry
        ));
    }
}
