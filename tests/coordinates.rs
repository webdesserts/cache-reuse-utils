//! Physical/logical coordinate helpers over sorted pad positions.

use cache_reuse_utils::{
    CommonPrefix, CoordinateMap, PlacedRange, append_unpadded, common_logical_prefix,
    logical_boundary, project_to_logical, slice_pad_positions, truncate_pad_positions,
    validate_pad_positions, verify_matches_logical, verify_span_correspondence,
};

/// Logical `[5, 6, 7]` with pads at physical 1 and 3.
const PHYSICAL: [u32; 5] = [5, 0, 6, 0, 7];
const PADS: [u32; 2] = [1, 3];

#[test]
fn pad_positions_must_be_strictly_increasing_and_in_bounds() {
    assert!(validate_pad_positions(&[], 0));
    assert!(validate_pad_positions(&[0, 1, 2], 3));
    assert!(!validate_pad_positions(&[0], 0));
    assert!(!validate_pad_positions(&[1, 1], 3));
    assert!(!validate_pad_positions(&[2, 1], 3));
    assert!(!validate_pad_positions(&[3], 3));
}

#[test]
fn projection_drops_pads_by_position_not_value() {
    // The second 0 is content: only positions 1 and 3 are pads.
    let physical = [5, 0, 0, 0, 7];
    let mut logical = [u32::MAX; 4];
    assert_eq!(project_to_logical(&physical, &PADS, &mut logical), Some(3));
    assert_eq!(logical, [5, 0, 7, u32::MAX]);
}

#[test]
fn projection_handles_edge_pads_and_no_pads() {
    let mut logical = [0; 3];
    assert_eq!(
        project_to_logical(&[9, 1, 2, 9], &[0, 3], &mut logical),
        Some(2)
    );
    assert_eq!(logical[..2], [1, 2]);
    assert_eq!(project_to_logical(&[1, 2, 3], &[], &mut logical), Some(3));
    assert_eq!(logical, [1, 2, 3]);
    assert_eq!(project_to_logical(&[9, 9], &[0, 1], &mut []), Some(0));
}

#[test]
fn projection_rejects_invalid_pads_or_a_short_output() {
    let mut logical = [0; 3];
    assert_eq!(project_to_logical(&PHYSICAL, &[3, 1], &mut logical), None);
    assert_eq!(project_to_logical(&PHYSICAL, &[5], &mut logical), None);
    assert_eq!(
        project_to_logical(&PHYSICAL, &PADS, &mut logical[..2]),
        None
    );
}

#[test]
fn boundaries_count_the_logical_tokens_before_them() {
    let expected = [0, 1, 1, 2, 2, 3];
    for (physical, logical) in expected.into_iter().enumerate() {
        assert_eq!(
            logical_boundary(physical as u32, 5, &PADS),
            Some(logical),
            "physical {physical}"
        );
    }
    assert_eq!(logical_boundary(6, 5, &PADS), None);
    assert_eq!(logical_boundary(2, 5, &[3, 1]), None);
}

#[test]
fn common_prefix_carries_only_pads_inside_the_matched_prefix() {
    let prefix = |logical: &[u32]| common_logical_prefix(logical, &PHYSICAL, &PADS).unwrap();
    let make = |common, carried| CommonPrefix {
        common_logical_tokens: common,
        physical_prefix_tokens: common + carried,
        carried_pads: carried,
        live_logical_tokens: 3,
    };
    assert_eq!(prefix(&[5, 6, 7, 8]), make(3, 2));
    assert_eq!(prefix(&[5, 6, 7]), make(3, 2));
    // The pad after 6 precedes the mismatch but is not carried.
    assert_eq!(prefix(&[5, 6, 8]), make(2, 1));
    assert_eq!(prefix(&[5, 6]), make(2, 1));
    assert_eq!(prefix(&[5]), make(1, 0));
    assert_eq!(prefix(&[4]), make(0, 0));
    assert_eq!(prefix(&[]), make(0, 0));
}

#[test]
fn common_prefix_carries_leading_pads_before_a_match() {
    let prefix = common_logical_prefix(&[1, 2], &[0, 0, 1, 2], &[0, 1]).unwrap();
    assert_eq!(
        prefix,
        CommonPrefix {
            common_logical_tokens: 2,
            physical_prefix_tokens: 4,
            carried_pads: 2,
            live_logical_tokens: 2,
        }
    );
    let none = common_logical_prefix(&[3], &[0, 0, 1, 2], &[0, 1]).unwrap();
    assert_eq!((none.physical_prefix_tokens, none.carried_pads), (0, 0));
}

#[test]
fn common_prefix_rejects_invalid_pads() {
    assert_eq!(common_logical_prefix(&[5], &PHYSICAL, &[1, 1]), None);
    assert_eq!(common_logical_prefix(&[5], &PHYSICAL, &[5]), None);
}

#[test]
fn coordinate_maps_are_validated_on_construction() {
    let map = CoordinateMap::new(5, PADS.to_vec()).unwrap();
    assert_eq!(
        (map.physical_len(), map.logical_len(), map.pad_positions()),
        (5, 3, &PADS[..])
    );
    assert_eq!(map.logical_boundary(4), Some(2));
    assert!(map.matches(&PHYSICAL, &[5, 6, 7]));
    assert!(!map.matches(&PHYSICAL[..4], &[5, 6]));
    assert_eq!(CoordinateMap::new(3, vec![1, 1]), None);
    assert_eq!(CoordinateMap::new(3, vec![3]), None);
    assert_eq!(CoordinateMap::new(0, vec![]).unwrap().logical_len(), 0);
}

#[test]
fn verify_matches_logical_compares_every_logical_token() {
    assert!(verify_matches_logical(&PHYSICAL, &PADS, &[5, 6, 7]));
    // A pad's value is irrelevant; only its position is.
    assert!(verify_matches_logical(&[5, 6, 6, 7, 7], &PADS, &[5, 6, 7]));
    assert!(verify_matches_logical(&[9, 9], &[0, 1], &[]));
    assert!(verify_matches_logical(&[], &[], &[]));
    assert!(!verify_matches_logical(&PHYSICAL, &PADS, &[5, 6, 8]));
    assert!(!verify_matches_logical(&PHYSICAL, &PADS, &[5, 6]));
    assert!(!verify_matches_logical(&PHYSICAL, &PADS, &[5, 6, 7, 0]));
    // With only position 1 a pad, the 0 at position 3 is content.
    assert!(verify_matches_logical(&PHYSICAL, &[1], &[5, 6, 0, 7]));
    assert!(!verify_matches_logical(&PHYSICAL, &[1], &[5, 0, 6, 7]));
    assert!(!verify_matches_logical(&PHYSICAL, &[3, 1], &[5, 6, 7]));
    assert!(!verify_matches_logical(&PHYSICAL, &[5], &[5, 0, 6, 0]));
}

#[test]
fn slicing_keeps_pads_inside_the_interval_relative_to_its_start() {
    // Pads at 1, 3 and 6 of an 8-token sequence.
    let pads = [1, 3, 6];
    let mut out = [u32::MAX; 3];
    assert_eq!(slice_pad_positions(&pads, 8, 2, 4, &mut out), Some(1));
    assert_eq!(out[0], 1);
    assert_eq!(slice_pad_positions(&pads, 8, 1, 6, &mut out), Some(3));
    assert_eq!(out, [0, 2, 5]);
    assert_eq!(slice_pad_positions(&pads, 8, 7, 1, &mut out), Some(0));
    assert_eq!(slice_pad_positions(&pads, 8, 8, 0, &mut out), Some(0));
    // Beyond the sequence, overflowing, invalid pads, or too little output.
    assert_eq!(slice_pad_positions(&pads, 8, 5, 4, &mut out), None);
    assert_eq!(slice_pad_positions(&pads, 8, u32::MAX, 2, &mut out), None);
    assert_eq!(slice_pad_positions(&[3, 1], 8, 0, 8, &mut out), None);
    assert_eq!(slice_pad_positions(&pads, 8, 0, 8, &mut out[..2]), None);

    let map = CoordinateMap::new(8, pads.to_vec()).unwrap();
    assert_eq!(map.slice(1, 6), CoordinateMap::new(6, vec![0, 2, 5]));
    assert_eq!(map.slice(2, 7), None);
}

#[test]
fn appending_adds_logical_tokens_without_pads() {
    assert_eq!(append_unpadded(5, &PADS, 3), Some(8));
    assert_eq!(append_unpadded(0, &[], 0), Some(0));
    assert_eq!(append_unpadded(u32::MAX - 1, &[], 1), Some(u32::MAX));
    assert_eq!(append_unpadded(u32::MAX, &[], 1), None);
    assert_eq!(append_unpadded(3, &[3], 1), None);

    let mut map = CoordinateMap::new(5, PADS.to_vec()).unwrap();
    assert_eq!(map.append_unpadded(2), Some(()));
    assert_eq!((map.physical_len(), map.logical_len()), (7, 5));
    let before = map.clone();
    assert_eq!(map.append_unpadded(u32::MAX), None);
    assert_eq!(map, before);
}

#[test]
fn truncation_keeps_pads_strictly_below_the_new_length() {
    assert_eq!(truncate_pad_positions(&PADS, 5, 5), Some(2));
    assert_eq!(truncate_pad_positions(&PADS, 5, 4), Some(2));
    assert_eq!(truncate_pad_positions(&PADS, 5, 3), Some(1));
    assert_eq!(truncate_pad_positions(&PADS, 5, 1), Some(0));
    assert_eq!(truncate_pad_positions(&PADS, 5, 0), Some(0));
    assert_eq!(truncate_pad_positions(&PADS, 5, 6), None);
    assert_eq!(truncate_pad_positions(&[5], 5, 2), None);

    let mut map = CoordinateMap::new(5, PADS.to_vec()).unwrap();
    assert_eq!(map.truncate(3), Some(()));
    assert_eq!(map, CoordinateMap::new(3, vec![1]).unwrap());
    assert_eq!(map.truncate(4), None);
    assert_eq!(map.physical_len(), 3);
}

fn span(source_start: u32, prepared_start: u32, token_count: u32) -> PlacedRange {
    PlacedRange {
        source_start,
        destination_start: 0,
        token_count,
        prepared_start,
    }
}

#[test]
fn span_correspondence_requires_the_same_relative_pads() {
    // Source pads at offsets 1 and 3 of a span starting at 2.
    let source = [3, 5, 9];
    assert!(verify_span_correspondence(
        &source,
        10,
        &[11, 13],
        20,
        span(2, 10, 5)
    ));
    // Pads outside either span are ignored.
    assert!(verify_span_correspondence(
        &source,
        10,
        &[0, 11, 13, 19],
        20,
        span(2, 10, 5)
    ));
    assert!(verify_span_correspondence(&[], 4, &[], 4, span(0, 0, 4)));
    // Different offset, missing or extra pad.
    assert!(!verify_span_correspondence(
        &source,
        10,
        &[11, 14],
        20,
        span(2, 10, 5)
    ));
    assert!(!verify_span_correspondence(
        &source,
        10,
        &[11],
        20,
        span(2, 10, 5)
    ));
    assert!(!verify_span_correspondence(
        &source,
        10,
        &[11, 12, 13],
        20,
        span(2, 10, 5)
    ));
}

#[test]
fn span_correspondence_rejects_pads_on_range_edges_and_bad_bounds() {
    // Matching pads, but on the first or last token of the range.
    assert!(!verify_span_correspondence(
        &[2],
        10,
        &[10],
        20,
        span(2, 10, 5)
    ));
    assert!(!verify_span_correspondence(
        &[6],
        10,
        &[14],
        20,
        span(2, 10, 5)
    ));
    assert!(!verify_span_correspondence(
        &[],
        10,
        &[],
        20,
        span(2, 10, 0)
    ));
    assert!(!verify_span_correspondence(
        &[],
        10,
        &[],
        20,
        span(6, 10, 5)
    ));
    assert!(!verify_span_correspondence(
        &[],
        10,
        &[],
        20,
        span(2, 16, 5)
    ));
    assert!(!verify_span_correspondence(
        &[],
        10,
        &[],
        20,
        span(u32::MAX, 0, 2)
    ));
    assert!(!verify_span_correspondence(
        &[4, 3],
        10,
        &[],
        20,
        span(0, 0, 2)
    ));
}
