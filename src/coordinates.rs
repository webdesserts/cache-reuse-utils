//! Stateless conversions between physical and logical token coordinates.
//!
//! A physical sequence is what an engine cache holds: logical tokens with pad
//! tokens interleaved. Pads are identified by position, never by token value,
//! through a strictly increasing `pad_positions` slice of physical indices.
//! The logical sequence is the physical one with those positions removed.
//! [`crate::PlacementRecipe::materialize_into`] produces such a slice for a
//! prepared target.

/// Result of [`common_logical_prefix`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommonPrefix {
    /// Leading logical tokens of `physical` equal to the start of `logical`.
    pub common_logical_tokens: u32,
    /// Physical tokens up to and including the last common logical token:
    /// `common_logical_tokens + carried_pads`.
    pub physical_prefix_tokens: u32,
    /// Pads interleaved inside that physical prefix. Pads after the last
    /// common logical token are not carried.
    pub carried_pads: u32,
    /// Logical (non-pad) tokens in the whole physical sequence.
    pub live_logical_tokens: u32,
}

/// Whether `pad_positions` is a valid pad set for a physical sequence of
/// `physical_len` tokens: strictly increasing and every position below
/// `physical_len`.
///
/// ```
/// # use cache_reuse_utils::validate_pad_positions;
/// assert!(validate_pad_positions(&[1, 3], 4));
/// assert!(!validate_pad_positions(&[3, 1], 4));
/// assert!(!validate_pad_positions(&[4], 4));
/// ```
pub fn validate_pad_positions(pad_positions: &[u32], physical_len: u32) -> bool {
    pad_positions.len() <= physical_len as usize
        && pad_positions.windows(2).all(|pair| pair[0] < pair[1])
        && pad_positions.last().is_none_or(|&last| last < physical_len)
}

/// Copy the logical tokens of `physical` into `out_logical` and return how
/// many were written.
///
/// Returns `None` for invalid pad positions, a physical length beyond `u32`,
/// or an `out_logical` shorter than the logical length. Runs in `O(N)`.
///
/// ```
/// # use cache_reuse_utils::project_to_logical;
/// let mut logical = [0; 4];
/// assert_eq!(project_to_logical(&[5, 0, 6, 0, 7], &[1, 3], &mut logical), Some(3));
/// assert_eq!(logical[..3], [5, 6, 7]);
/// ```
pub fn project_to_logical(
    physical: &[u32],
    pad_positions: &[u32],
    out_logical: &mut [u32],
) -> Option<u32> {
    let physical_len = u32::try_from(physical.len()).ok()?;
    if !validate_pad_positions(pad_positions, physical_len) {
        return None;
    }
    let logical_len = physical.len() - pad_positions.len();
    let out = out_logical.get_mut(..logical_len)?;
    let mut written = 0;
    let mut start = 0;
    for &pad in pad_positions.iter().chain([&physical_len]) {
        let run = &physical[start..pad as usize];
        out[written..written + run.len()].copy_from_slice(run);
        written += run.len();
        start = pad as usize + 1;
    }
    Some(logical_len as u32)
}

/// Map a physical boundary to the logical boundary at the same place: the
/// number of logical tokens before it.
///
/// Returns `None` when `physical_boundary > physical_len` or the pad
/// positions are invalid. Validation is `O(P)`; the lookup is `O(log P)`.
///
/// ```
/// # use cache_reuse_utils::logical_boundary;
/// assert_eq!(logical_boundary(3, 5, &[1, 3]), Some(2));
/// assert_eq!(logical_boundary(4, 5, &[1, 3]), Some(2));
/// ```
pub fn logical_boundary(
    physical_boundary: u32,
    physical_len: u32,
    pad_positions: &[u32],
) -> Option<u32> {
    if physical_boundary > physical_len || !validate_pad_positions(pad_positions, physical_len) {
        return None;
    }
    let pads_before = pad_positions.partition_point(|&pad| pad < physical_boundary);
    Some(physical_boundary - pads_before as u32)
}

/// Measure how much of `logical` a cached `physical` sequence already holds
/// as a prefix, ignoring its pads.
///
/// Walks `physical`, skipping pad positions, while its logical tokens equal
/// `logical` in order. The physical prefix ends right after the last equal
/// token, so it can be kept as-is: its interleaved pads are carried, and
/// pads after it are not. Returns `None` for invalid pad positions or a
/// physical length beyond `u32`.
///
/// ```
/// # use cache_reuse_utils::{CommonPrefix, common_logical_prefix};
/// let prefix = common_logical_prefix(&[5, 6, 8], &[5, 0, 6, 0, 7], &[1, 3]).unwrap();
/// assert_eq!(
///     prefix,
///     CommonPrefix {
///         common_logical_tokens: 2,
///         physical_prefix_tokens: 3,
///         carried_pads: 1,
///         live_logical_tokens: 3,
///     }
/// );
/// ```
pub fn common_logical_prefix(
    logical: &[u32],
    physical: &[u32],
    pad_positions: &[u32],
) -> Option<CommonPrefix> {
    let physical_len = u32::try_from(physical.len()).ok()?;
    if !validate_pad_positions(pad_positions, physical_len) {
        return None;
    }
    let mut pads = pad_positions.iter().peekable();
    let mut common = 0usize;
    let mut carried = 0u32;
    let mut pending = 0u32;
    for (index, &token) in physical.iter().enumerate() {
        if pads.next_if(|&&pad| pad as usize == index).is_some() {
            pending += 1;
            continue;
        }
        if logical.get(common) != Some(&token) {
            break;
        }
        common += 1;
        carried += pending;
        pending = 0;
    }
    let common = common as u32;
    Some(CommonPrefix {
        common_logical_tokens: common,
        physical_prefix_tokens: common + carried,
        carried_pads: carried,
        live_logical_tokens: physical_len - pad_positions.len() as u32,
    })
}
