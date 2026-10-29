//! Separator boundary discovery and authorization of pad maps.

use cache_reuse_utils::{SeparatorPolicy, collect_separator_boundaries, verify_separator_map};

/// Token 0 is a space and 1 a newline; other tokens are words.
const SEPARATORS: [u32; 2] = [0, 1];
const PAD: u32 = 0;

#[test]
fn boundaries_sit_before_or_after_a_separator() {
    // "a b\nc" -> [a, sp, b, nl, c]
    let logical = [7, 0, 8, 1, 9];
    let mut out = [u32::MAX; 8];
    assert_eq!(
        collect_separator_boundaries(&logical, 0, &SEPARATORS, &mut out),
        4
    );
    assert_eq!(out[..4], [1, 2, 3, 4]);
    // Words with no separator between them never qualify; the end is no boundary.
    assert_eq!(
        collect_separator_boundaries(&[7, 8, 9], 0, &SEPARATORS, &mut out),
        0
    );
    assert_eq!(
        collect_separator_boundaries(&[7, 0], 0, &SEPARATORS, &mut out),
        1
    );
    assert_eq!(out[0], 1);
    // A leading separator makes boundary 0 qualify.
    assert_eq!(
        collect_separator_boundaries(&[0, 7, 8], 0, &SEPARATORS, &mut out),
        2
    );
    assert_eq!(out[..2], [0, 1]);
}

#[test]
fn boundary_collection_honours_start_and_output_capacity() {
    let logical = [7, 0, 8, 1, 9];
    let mut out = [u32::MAX; 2];
    assert_eq!(
        collect_separator_boundaries(&logical, 3, &SEPARATORS, &mut out),
        2
    );
    assert_eq!(out, [3, 4]);
    assert_eq!(
        collect_separator_boundaries(&logical, 0, &SEPARATORS, &mut out),
        2
    );
    assert_eq!(out, [1, 2]);
    assert_eq!(
        collect_separator_boundaries(&logical, 9, &SEPARATORS, &mut out),
        0
    );
    assert_eq!(collect_separator_boundaries(&logical, 0, &[], &mut out), 0);
    assert_eq!(
        collect_separator_boundaries(&logical, 0, &SEPARATORS, &mut []),
        0
    );
}

#[test]
fn policy_tracks_at_most_its_bound() {
    let policy = SeparatorPolicy {
        separator_tokens: &SEPARATORS,
        max_tracked_boundaries: 3,
    };
    assert!(policy.is_separator(1) && !policy.is_separator(7));
    assert_eq!(policy.boundaries(&[7, 0, 8, 1, 9], 0), [1, 2, 3]);
    assert_eq!(policy.boundaries(&[7, 0, 8, 1, 9], 2), [2, 3, 4]);
    let unbounded = SeparatorPolicy {
        max_tracked_boundaries: usize::MAX,
        ..policy
    };
    assert_eq!(unbounded.boundaries(&[7, 0, 8], 0), [1, 2]);
}

/// `logical` with `pad_token` at the physical indices in `pads`.
fn padded(logical: &[u32], pads: &[u32], pad_token: u32) -> Vec<u32> {
    let mut physical = Vec::new();
    let mut source = logical.iter();
    for index in 0..(logical.len() + pads.len()) as u32 {
        physical.push(if pads.contains(&index) {
            pad_token
        } else {
            *source.next().unwrap()
        });
    }
    physical
}

#[test]
fn new_pads_must_hold_the_pad_token_at_separator_boundaries() {
    let logical = [7, 0, 8, 0, 9];
    // Two pads after the first space: boundary 2, before 8.
    let physical = padded(&logical, &[2, 3], PAD);
    assert!(verify_separator_map(
        &logical,
        &physical,
        &[2, 3],
        0,
        &[],
        PAD,
        &SEPARATORS
    ));
    assert!(verify_separator_map(
        &logical,
        &logical,
        &[],
        0,
        &[],
        PAD,
        &SEPARATORS
    ));

    // Between two words: boundary 1 of [7, 8, 0] is not authorized.
    let words = [7, 8, 0];
    let physical = padded(&words, &[1], PAD);
    assert!(!verify_separator_map(
        &words,
        &physical,
        &[1],
        0,
        &[],
        PAD,
        &SEPARATORS
    ));
    // A trailing pad (the end is not a boundary).
    let physical = padded(&logical, &[5], PAD);
    assert!(!verify_separator_map(
        &logical,
        &physical,
        &[5],
        0,
        &[],
        PAD,
        &SEPARATORS
    ));
    // The right place, but the wrong token.
    let physical = padded(&logical, &[2], 1);
    assert!(!verify_separator_map(
        &logical,
        &physical,
        &[2],
        0,
        &[],
        PAD,
        &SEPARATORS
    ));
    // Pads that do not project back to the logical sequence.
    let physical = padded(&logical, &[2], PAD);
    assert!(!verify_separator_map(
        &[7, 0, 8, 0],
        &physical,
        &[2],
        0,
        &[],
        PAD,
        &SEPARATORS
    ));
}

#[test]
fn carried_pads_are_trusted_but_must_match_exactly() {
    // Logical [7, 8, 0, 9]: a carried pad at physical 1 sits between two
    // words, so only the carried-prefix rule can authorize it.
    let logical = [7, 8, 0, 9];
    let physical = padded(&logical, &[1, 4], PAD);
    assert!(verify_separator_map(
        &logical,
        &physical,
        &[1, 4],
        3,
        &[1],
        PAD,
        &SEPARATORS
    ));
    // Carried pads keep whatever token they had.
    let mut carried_value = physical.clone();
    carried_value[1] = 42;
    assert!(verify_separator_map(
        &logical,
        &carried_value,
        &[1, 4],
        3,
        &[1],
        PAD,
        &SEPARATORS
    ));
    // Without the carry, the word-internal pad is refused.
    assert!(!verify_separator_map(
        &logical,
        &physical,
        &[1, 4],
        0,
        &[],
        PAD,
        &SEPARATORS
    ));
    // A carried prefix holding a pad that is not listed, or listing another.
    assert!(!verify_separator_map(
        &logical,
        &physical,
        &[1, 4],
        3,
        &[],
        PAD,
        &SEPARATORS
    ));
    assert!(!verify_separator_map(
        &logical,
        &physical,
        &[1, 4],
        3,
        &[2],
        PAD,
        &SEPARATORS
    ));
    // Carried pads must lie inside the prefix, which must fit the sequence.
    assert!(!verify_separator_map(
        &logical,
        &physical,
        &[1, 4],
        1,
        &[1],
        PAD,
        &SEPARATORS
    ));
    assert!(!verify_separator_map(
        &logical,
        &physical,
        &[1, 4],
        7,
        &[1],
        PAD,
        &SEPARATORS
    ));
}
