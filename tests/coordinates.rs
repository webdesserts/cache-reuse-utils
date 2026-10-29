//! Physical/logical coordinate helpers over sorted pad positions.

use cache_reuse_utils::{
    CommonPrefix, common_logical_prefix, logical_boundary, project_to_logical,
    validate_pad_positions,
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
