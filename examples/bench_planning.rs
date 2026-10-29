//! Planning-helper benchmarks on realistic long-context workloads.
//!
//! Run with `cargo run --release --example bench_planning -j 2`. Each case
//! reports the median and minimum of repeated runs over deterministic
//! synthetic data, plus token throughput.

use std::hint::black_box;
use std::time::{Duration, Instant};

use cache_reuse_utils::{
    ArchiveGeometry, CandidateInput, CandidateSelectionPolicy, PlacedRange,
    collect_separator_boundaries, compile_archive_schedule, select_candidate,
    verify_archive_schedule, verify_matches_logical, verify_separator_map,
};

const SIZES: [u32; 3] = [32_768, 131_072, 262_144];
/// Eight separator IDs, e.g. a tokenizer's space and newline variants.
const SEPARATORS: [u32; 8] = [3, 17, 220, 198, 271, 1_011, 2_048, 30_000];
const PAD: u32 = 3;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }
}

/// Text-like tokens: about one in eight is a separator.
fn logical_tokens(len: u32, seed: u64) -> Vec<u32> {
    let mut rng = Rng(seed);
    (0..len)
        .map(|_| {
            let r = rng.next();
            if r.is_multiple_of(8) {
                SEPARATORS[(r / 8) as usize % SEPARATORS.len()]
            } else {
                40_000 + r % 60_000
            }
        })
        .collect()
}

/// Insert `pads` pads, spread evenly, each right after a separator, and
/// return the physical tokens and pad positions.
fn padded(logical: &[u32], pads: usize) -> (Vec<u32>, Vec<u32>) {
    let stride = (logical.len() / (pads + 1)).max(1);
    let mut physical = Vec::with_capacity(logical.len() + pads);
    let mut positions = Vec::with_capacity(pads);
    let mut next_target = stride;
    for (index, &token) in logical.iter().enumerate() {
        if positions.len() < pads
            && index >= next_target
            && index > 0
            && SEPARATORS.contains(&logical[index - 1])
        {
            positions.push(physical.len() as u32);
            physical.push(PAD);
            next_target += stride;
        }
        physical.push(token);
    }
    (physical, positions)
}

/// Median and minimum of `iters` timed runs after one warm-up.
fn time(iters: usize, mut run: impl FnMut()) -> (Duration, Duration) {
    run();
    let mut samples: Vec<Duration> = (0..iters)
        .map(|_| {
            let start = Instant::now();
            run();
            start.elapsed()
        })
        .collect();
    samples.sort();
    (samples[samples.len() / 2], samples[0])
}

fn report(case: &str, tokens: u64, (median, min): (Duration, Duration)) {
    let throughput = tokens as f64 / median.as_secs_f64() / 1e6;
    println!(
        "| {case:<46} | {:>10.1} | {:>10.1} | {throughput:>10.0} |",
        median.as_secs_f64() * 1e6,
        min.as_secs_f64() * 1e6,
    );
}

/// Per-token reference: the element-wise walk the slice-run version replaces.
fn verify_per_token(physical: &[u32], pads: &[u32], logical: &[u32]) -> bool {
    if physical.len() - pads.len() != logical.len() {
        return false;
    }
    let mut pads = pads.iter().peekable();
    let mut written = 0;
    for (index, &token) in physical.iter().enumerate() {
        if pads.next_if(|&&pad| pad as usize == index).is_some() {
            continue;
        }
        if logical[written] != token {
            return false;
        }
        written += 1;
    }
    true
}

fn bench_coordinates(len: u32, iters: usize) {
    let logical = logical_tokens(len, 1);
    let pads = (500 * len as usize / 262_144).max(1);
    let (physical, positions) = padded(&logical, pads);
    assert_eq!(positions.len(), pads);
    let n = physical.len() as u64;
    report(
        &format!("verify_matches_logical {len} ({pads} pads)"),
        n,
        time(iters, || {
            assert!(verify_matches_logical(
                black_box(&physical),
                black_box(&positions),
                black_box(&logical)
            ));
        }),
    );
    report(
        &format!("  reference per-token walk {len}"),
        n,
        time(iters, || {
            assert!(verify_per_token(
                black_box(&physical),
                black_box(&positions),
                black_box(&logical)
            ));
        }),
    );

    let mut out = vec![0u32; logical.len()];
    let mut found = 0;
    report(
        &format!("collect_separator_boundaries {len}"),
        len as u64,
        time(iters, || {
            found = collect_separator_boundaries(black_box(&logical), 0, &SEPARATORS, &mut out);
        }),
    );
    assert!(found > len / 8);
    report(
        &format!("verify_separator_map {len}"),
        n,
        time(iters, || {
            assert!(verify_separator_map(
                black_box(&logical),
                black_box(&physical),
                black_box(&positions),
                0,
                &[],
                PAD,
                &SEPARATORS,
            ));
        }),
    );
}

fn bench_selection(len: u32, iters: usize) {
    let target = logical_tokens(len, 7);
    // Sixteen candidates sharing progressively longer prefixes of the target.
    let candidates: Vec<(Vec<u32>, Vec<u32>)> = (1..=16u32)
        .map(|i| {
            let shared = (len as u64 * i as u64 / 17) as usize;
            let mut logical = target[..shared].to_vec();
            logical.extend(logical_tokens(len - shared as u32, 100 + i as u64));
            padded(&logical, (500 * len as usize / 262_144).max(1))
        })
        .collect();
    let pool: Vec<_> = candidates
        .iter()
        .map(|(physical, pads)| CandidateInput {
            physical,
            pad_positions: pads,
            planned_reusable_tokens: 0,
        })
        .collect();
    let policy = CandidateSelectionPolicy {
        minimum_match_tokens: 128,
        alignment: 8,
        restore_penalty_tokens: 512,
    };
    // Each common-prefix walk stops at its candidate's first mismatch.
    let scanned: u64 = (1..=16u64).map(|i| len as u64 * i / 17).sum();
    report(
        &format!("select_candidate 16 x {len}"),
        scanned,
        time(iters, || {
            let selection =
                select_candidate(black_box(&target), None, black_box(&pool), policy).unwrap();
            black_box(selection);
        }),
    );
}

fn bench_schedule(len: u32, iters: usize) {
    // 32 ranges of about 2/3 of each 1/33 slot, aligned to 8.
    let slot = len / 33;
    let ranges: Vec<PlacedRange> = (0..32u32)
        .map(|i| {
            let start = (slot * i + slot / 3) / 8 * 8;
            PlacedRange {
                source_start: start + 1_000,
                destination_start: start,
                token_count: (slot * 2 / 3) / 8 * 8,
                prepared_start: start,
            }
        })
        .collect();
    let geometry = ArchiveGeometry {
        raw_arena_tokens: 2_048,
        max_import_chunk: 128,
        encoder_acquire_threshold: 1_024,
        encoder_release_tail: 768,
        head_alignment: 8,
    };
    let mut ops = Vec::new();
    report(
        &format!("compile_archive_schedule 32 ranges {len}"),
        len as u64,
        time(iters, || {
            ops =
                compile_archive_schedule(black_box(len - 3), black_box(&ranges), geometry).unwrap();
        }),
    );
    report(
        &format!("  verify_archive_schedule ({} ops)", ops.len()),
        len as u64,
        time(iters, || {
            assert!(verify_archive_schedule(
                black_box(&ops),
                len - 3,
                &ranges,
                geometry
            ));
        }),
    );
}

fn main() {
    let iters = if std::env::args().any(|arg| arg == "--quick") {
        5
    } else {
        31
    };
    println!("| case | median us | min us | Mtok/s |");
    println!("|---|---:|---:|---:|");
    for len in SIZES {
        bench_coordinates(len, iters);
        bench_selection(len, iters.min(11));
        bench_schedule(len, iters);
    }
}
