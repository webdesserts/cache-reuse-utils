//! Separator boundaries: where a pad may be inserted into a logical sequence.
//!
//! A boundary `i` is the gap before `logical[i]`. It is a separator boundary
//! when `logical[i]` or `logical[i - 1]` is a separator token, so a pad there
//! sits next to a separator rather than inside a run of ordinary tokens.
//! Which tokens are separators is the caller's choice, for example the
//! tokenizer's whitespace and newline tokens.

use crate::coordinates::{validate_pad_positions, verify_matches_logical};

/// The separator tokens that authorize pad insertion, and a bound on the
/// boundaries a caller tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeparatorPolicy<'a> {
    /// Separator token IDs. Membership is a linear scan, so keep this short.
    pub separator_tokens: &'a [u32],
    /// Most boundaries [`SeparatorPolicy::boundaries`] returns.
    pub max_tracked_boundaries: usize,
}

impl SeparatorPolicy<'_> {
    /// Whether `token` is a separator.
    pub fn is_separator(&self, token: u32) -> bool {
        self.separator_tokens.contains(&token)
    }

    /// The first `max_tracked_boundaries` separator boundaries at or after
    /// `start_index`. See [`collect_separator_boundaries`].
    pub fn boundaries(&self, logical: &[u32], start_index: u32) -> Vec<u32> {
        let cap = self.max_tracked_boundaries.min(logical.len());
        let mut out = vec![0; cap];
        let written =
            collect_separator_boundaries(logical, start_index, self.separator_tokens, &mut out);
        out.truncate(written as usize);
        out
    }
}

/// Separator membership with a 1,024-bit prefilter on the token's low bits:
/// most ordinary tokens are rejected by one bit test before the linear scan.
struct SeparatorSet<'a> {
    separators: &'a [u32],
    filter: [u64; 16],
}

impl<'a> SeparatorSet<'a> {
    fn new(separators: &'a [u32]) -> Self {
        let mut filter = [0; 16];
        for &token in separators {
            filter[(token >> 6) as usize & 15] |= 1 << (token & 63);
        }
        Self { separators, filter }
    }

    #[inline]
    fn contains(&self, token: u32) -> bool {
        (self.filter[(token >> 6) as usize & 15] >> (token & 63)) & 1 != 0
            && self.separators.contains(&token)
    }
}

fn at_separator(logical: &[u32], index: usize, separators: &[u32]) -> bool {
    let is_separator = |i: usize| {
        logical
            .get(i)
            .is_some_and(|token| separators.contains(token))
    };
    is_separator(index) || (index > 0 && is_separator(index - 1))
}

/// Write the separator boundaries `i` in `start_index..logical.len()` to
/// `out_boundaries` in increasing order, and return how many were written.
///
/// `i` qualifies when `logical[i]` or `logical[i - 1]` is in `separators`.
/// The end of the sequence is not a boundary. Stops when `out_boundaries`
/// is full. Tests each token once: a one-bit prefilter, then a scan of the
/// `S` separators for tokens that pass it. Output slots past the returned
/// count may be overwritten.
///
/// ```
/// # use cache_reuse_utils::collect_separator_boundaries;
/// let mut out = [0; 8];
/// // 0 is the separator.
/// assert_eq!(collect_separator_boundaries(&[5, 0, 6, 7, 0], 0, &[0], &mut out), 3);
/// assert_eq!(out[..3], [1, 2, 4]);
/// ```
pub fn collect_separator_boundaries(
    logical: &[u32],
    start_index: u32,
    separators: &[u32],
    out_boundaries: &mut [u32],
) -> u32 {
    let set = SeparatorSet::new(separators);
    let mut written = 0;
    let start = (start_index as usize).min(logical.len());
    // Each token is tested once; its result is the next index's `prev`.
    let mut prev_is_separator = start > 0 && set.contains(logical[start - 1]);
    let mut index = start;
    // Branch-free while every remaining index fits: write each index and keep
    // it by advancing `written` only at a boundary.
    while index < logical.len() && logical.len() - index <= out_boundaries.len() - written {
        let is_separator = set.contains(logical[index]);
        out_boundaries[written] = index as u32;
        written += usize::from(is_separator || prev_is_separator);
        prev_is_separator = is_separator;
        index += 1;
    }
    while index < logical.len() && written < out_boundaries.len() {
        let is_separator = set.contains(logical[index]);
        if is_separator || prev_is_separator {
            out_boundaries[written] = index as u32;
            written += 1;
        }
        prev_is_separator = is_separator;
        index += 1;
    }
    written as u32
}

/// Whether a physical sequence is `logical` with only authorized pads.
///
/// Holds when all of these do:
/// 1. removing `pad_positions` from `physical` leaves `logical`;
/// 2. the pads below `carried_physical_prefix` are exactly `carried_pads`,
///    a kept prefix whose pads were authorized earlier;
/// 3. every later pad holds `pad_token` and sits at a separator boundary
///    of `logical` (see [`collect_separator_boundaries`]).
///
/// Runs in `O(N + P * S)` without allocating.
///
/// ```
/// # use cache_reuse_utils::verify_separator_map;
/// // Logical [5, 0, 6] with 0 as the separator; pad 9 inserted after it.
/// assert!(verify_separator_map(&[5, 0, 6], &[5, 0, 9, 6], &[2], 0, &[], 9, &[0]));
/// assert!(!verify_separator_map(&[5, 6, 0], &[5, 9, 6, 0], &[1], 0, &[], 9, &[0]));
/// ```
pub fn verify_separator_map(
    logical: &[u32],
    physical: &[u32],
    pad_positions: &[u32],
    carried_physical_prefix: u32,
    carried_pads: &[u32],
    pad_token: u32,
    separators: &[u32],
) -> bool {
    if !verify_matches_logical(physical, pad_positions, logical)
        || carried_physical_prefix as usize > physical.len()
        || !validate_pad_positions(carried_pads, carried_physical_prefix)
    {
        return false;
    }
    let carried = carried_pads.len();
    let kept_prefix = pad_positions.partition_point(|&pad| pad < carried_physical_prefix);
    if kept_prefix != carried || pad_positions[..carried] != *carried_pads {
        return false;
    }
    pad_positions[carried..]
        .iter()
        .enumerate()
        .all(|(offset, &pad)| {
            // Logical tokens before this pad: its physical index less the pads before it.
            let boundary = pad as usize - (carried + offset);
            physical[pad as usize] == pad_token
                && boundary < logical.len()
                && at_separator(logical, boundary, separators)
        })
}
