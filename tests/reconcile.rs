//! Clipping planned ranges to an archive and reconciling executed ranges.

use cache_reuse_utils::{
    ExecutionSummary, PlacedRange, clip_ranges_to_archive, reconcile_executed_ranges,
};

/// A range whose destination is shifted by `pads` inserted before it.
fn range(source_start: u32, destination_start: u32, token_count: u32, pads: u32) -> PlacedRange {
    PlacedRange {
        source_start,
        destination_start,
        token_count,
        prepared_start: destination_start + pads,
    }
}

fn clip(
    ranges: &[PlacedRange],
    start: u32,
    end: u32,
    cap: u32,
    alignment: u32,
) -> Option<Vec<PlacedRange>> {
    let mut out = vec![range(0, 0, 0, 0); ranges.len()];
    let written = clip_ranges_to_archive(ranges, start, end, cap, alignment, &mut out)?;
    out.truncate(written as usize);
    Some(out)
}

#[test]
fn clipping_caps_destinations_below_the_aligned_target_cap() {
    let ranges = [
        range(0, 8, 16, 0),
        range(40, 32, 16, 2),
        range(80, 60, 8, 2),
    ];
    // Cap 43 aligns down to 40: the second range keeps 40 - 34 = 6 -> 4
    // tokens; the third starts beyond it.
    assert_eq!(
        clip(&ranges, 0, 1000, 43, 4).unwrap(),
        [range(0, 8, 16, 0), range(40, 32, 4, 2)]
    );
    // Without a binding cap nothing changes.
    assert_eq!(clip(&ranges, 0, 1000, 1000, 2).unwrap(), ranges);
}

#[test]
fn clipping_trims_sources_to_the_archive_by_aligned_amounts() {
    let ranges = [range(10, 10, 20, 0), range(50, 40, 20, 0)];
    // Archive [13, 60): the first range loses 4 aligned tokens at the front
    // (3 rounded up), the second keeps 10 source tokens before 60.
    assert_eq!(
        clip(&ranges, 13, 60, 1000, 2).unwrap(),
        [range(14, 14, 16, 0), range(50, 40, 10, 0)]
    );
    // Ranges entirely outside the archive are dropped.
    assert_eq!(clip(&ranges, 30, 50, 1000, 2).unwrap(), []);
    assert_eq!(clip(&ranges, 0, 0, 1000, 2).unwrap(), []);
}

#[test]
fn clipping_rejects_invalid_requests() {
    let ranges = [range(0, 0, 8, 0), range(10, 10, 8, 0)];
    assert_eq!(clip(&ranges, 0, 100, 100, 0), None);
    assert_eq!(clip(&ranges, 50, 40, 100, 2), None);
    assert_eq!(clip(&[ranges[1], ranges[0]], 0, 100, 100, 2), None);
    assert_eq!(clip(&[range(0, 0, 0, 0)], 0, 100, 100, 2), None);
    let mut short = [range(0, 0, 0, 0); 1];
    assert_eq!(
        clip_ranges_to_archive(&ranges, 0, 100, 100, 2, &mut short),
        None
    );
    // A short output is fine once clipping drops enough.
    assert_eq!(
        clip_ranges_to_archive(&ranges, 0, 100, 9, 2, &mut short),
        Some(1)
    );
}

const PLANNED: [PlacedRange; 3] = [
    PlacedRange {
        source_start: 0,
        destination_start: 8,
        token_count: 16,
        prepared_start: 8,
    },
    PlacedRange {
        source_start: 40,
        destination_start: 32,
        token_count: 16,
        prepared_start: 34,
    },
    PlacedRange {
        source_start: 80,
        destination_start: 60,
        token_count: 8,
        prepared_start: 64,
    },
];

#[test]
fn reconciliation_accounts_for_every_physical_token() {
    // The middle range was skipped and the last one stopped after 4 tokens.
    let executed = [PLANNED[0], range(80, 60, 4, 4)];
    let summary = reconcile_executed_ranges(&PLANNED, &executed, 80, 2).unwrap();
    assert_eq!(
        summary,
        ExecutionSummary {
            reused_tokens: 20,
            gap_tokens: 40,
            suffix_tokens: 12,
            fresh_tokens: 60,
            first_reused_start: 8,
            covered_end: 68,
        }
    );
    assert_eq!(
        summary.first_reused_start
            + summary.gap_tokens
            + summary.reused_tokens
            + summary.suffix_tokens,
        80
    );
    let all = reconcile_executed_ranges(&PLANNED, &PLANNED, 80, 8).unwrap();
    assert_eq!(
        (all.reused_tokens, all.gap_tokens, all.covered_end),
        (40, 24, 72)
    );
}

#[test]
fn nothing_executed_is_all_fresh() {
    assert_eq!(
        reconcile_executed_ranges(&PLANNED, &[], 80, 2),
        Some(ExecutionSummary {
            reused_tokens: 0,
            gap_tokens: 0,
            suffix_tokens: 0,
            fresh_tokens: 80,
            first_reused_start: 80,
            covered_end: 80,
        })
    );
    assert!(reconcile_executed_ranges(&[], &[], 0, 1).is_some());
}

#[test]
fn executed_ranges_must_be_an_ordered_aligned_subset_of_the_plan() {
    let check = |executed: &[PlacedRange]| reconcile_executed_ranges(&PLANNED, executed, 80, 4);
    // Moved, grown, out of order, repeated, unaligned or empty.
    assert_eq!(check(&[range(2, 8, 16, 0)]), None);
    assert_eq!(check(&[range(0, 8, 16, 2)]), None);
    assert_eq!(check(&[range(0, 8, 20, 0)]), None);
    assert_eq!(check(&[PLANNED[1], PLANNED[0]]), None);
    assert_eq!(check(&[PLANNED[0], PLANNED[0]]), None);
    assert_eq!(check(&[range(0, 8, 6, 0)]), None);
    assert_eq!(check(&[range(0, 8, 0, 0)]), None);
    // The plan must fit the physical length, and alignment must be nonzero.
    assert_eq!(reconcile_executed_ranges(&PLANNED, &[], 71, 4), None);
    assert_eq!(reconcile_executed_ranges(&PLANNED, &[], 80, 0), None);
}
