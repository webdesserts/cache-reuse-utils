//! Stateless conversions between physical and logical token coordinates.
//!
//! A physical sequence is what an engine cache holds: logical tokens with pad
//! tokens interleaved. Pads are identified by position, never by token value,
//! through a strictly increasing `pad_positions` slice of physical indices.
//! The logical sequence is the physical one with those positions removed.
//! [`crate::PlacementRecipe::materialize_into`] produces such a slice for a
//! prepared target.

use crate::placement::PlacedRange;

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
    // Compare whole runs between pads; pads before a matched token are carried.
    let mut common = 0usize;
    let mut carried = 0u32;
    let mut pending = 0u32;
    let mut start = 0;
    for &pad in pad_positions.iter().chain([&physical_len]) {
        let run = &physical[start..pad as usize];
        let matched = matching_prefix(run, &logical[common.min(logical.len())..]);
        if matched > 0 {
            common += matched;
            carried += pending;
            pending = 0;
        }
        if matched < run.len() {
            break;
        }
        pending += 1;
        start = pad as usize + 1;
    }
    let common = common as u32;
    Some(CommonPrefix {
        common_logical_tokens: common,
        physical_prefix_tokens: common + carried,
        carried_pads: carried,
        live_logical_tokens: physical_len - pad_positions.len() as u32,
    })
}

/// Length of the common prefix of `a` and `b`, comparing fixed blocks first
/// so equal runs compile to wide compares.
fn matching_prefix(a: &[u32], b: &[u32]) -> usize {
    const BLOCK: usize = 64;
    let len = a.len().min(b.len());
    let mut done = 0;
    while done + BLOCK <= len && a[done..done + BLOCK] == b[done..done + BLOCK] {
        done += BLOCK;
    }
    done + a[done..len]
        .iter()
        .zip(&b[done..len])
        .take_while(|(x, y)| x == y)
        .count()
}

/// An owned physical sequence shape: its length and validated pad positions.
///
/// Holds coordinates only, never tokens. Every constructor and update keeps
/// the pad positions strictly increasing and below the physical length.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CoordinateMap {
    physical_len: u32,
    pad_positions: Vec<u32>,
}

impl CoordinateMap {
    /// Returns `None` unless `pad_positions` is valid for `physical_len`.
    ///
    /// ```
    /// # use cache_reuse_utils::CoordinateMap;
    /// let map = CoordinateMap::new(5, vec![1, 3]).unwrap();
    /// assert_eq!(map.logical_len(), 3);
    /// assert!(CoordinateMap::new(3, vec![3]).is_none());
    /// ```
    pub fn new(physical_len: u32, pad_positions: Vec<u32>) -> Option<Self> {
        validate_pad_positions(&pad_positions, physical_len).then_some(Self {
            physical_len,
            pad_positions,
        })
    }

    /// Physical tokens, pads included.
    pub fn physical_len(&self) -> u32 {
        self.physical_len
    }

    /// Strictly increasing physical indices of the pads.
    pub fn pad_positions(&self) -> &[u32] {
        &self.pad_positions
    }

    /// Logical (non-pad) tokens.
    pub fn logical_len(&self) -> u32 {
        self.physical_len - self.pad_positions.len() as u32
    }

    /// See [`logical_boundary`].
    pub fn logical_boundary(&self, physical_boundary: u32) -> Option<u32> {
        logical_boundary(physical_boundary, self.physical_len, &self.pad_positions)
    }

    /// Whether removing this map's pads from `physical` leaves `logical`.
    /// See [`verify_matches_logical`]; `physical` must also have this map's
    /// length.
    pub fn matches(&self, physical: &[u32], logical: &[u32]) -> bool {
        physical.len() == self.physical_len as usize
            && verify_matches_logical(physical, &self.pad_positions, logical)
    }

    /// The map of the physical interval `[start, start + len)`, with pad
    /// positions relative to `start`. See [`slice_pad_positions`].
    pub fn slice(&self, start: u32, len: u32) -> Option<Self> {
        let end = start.checked_add(len)?;
        if end > self.physical_len {
            return None;
        }
        let first = self.pad_positions.partition_point(|&pad| pad < start);
        let last = self.pad_positions.partition_point(|&pad| pad < end);
        let pad_positions = self.pad_positions[first..last]
            .iter()
            .map(|&pad| pad - start)
            .collect();
        Some(Self {
            physical_len: len,
            pad_positions,
        })
    }

    /// Append `appended_tokens` logical tokens. Returns `None`, leaving the
    /// map unchanged, if the length would overflow `u32`.
    pub fn append_unpadded(&mut self, appended_tokens: u32) -> Option<()> {
        self.physical_len =
            append_unpadded(self.physical_len, &self.pad_positions, appended_tokens)?;
        Some(())
    }

    /// Keep the first `new_physical_len` physical tokens and their pads.
    /// Returns `None`, leaving the map unchanged, if that is longer than the
    /// map.
    pub fn truncate(&mut self, new_physical_len: u32) -> Option<()> {
        let kept =
            truncate_pad_positions(&self.pad_positions, self.physical_len, new_physical_len)?;
        self.pad_positions.truncate(kept);
        self.physical_len = new_physical_len;
        Some(())
    }
}

/// Whether removing `pad_positions` from `physical` leaves exactly
/// `logical`, token for token.
///
/// False for invalid pad positions or a physical length beyond `u32`. Runs
/// in `O(N)` without allocating.
///
/// ```
/// # use cache_reuse_utils::verify_matches_logical;
/// assert!(verify_matches_logical(&[5, 0, 6, 0, 7], &[1, 3], &[5, 6, 7]));
/// assert!(!verify_matches_logical(&[5, 0, 6, 0, 7], &[1], &[5, 6, 7]));
/// ```
pub fn verify_matches_logical(physical: &[u32], pad_positions: &[u32], logical: &[u32]) -> bool {
    let Ok(physical_len) = u32::try_from(physical.len()) else {
        return false;
    };
    if !validate_pad_positions(pad_positions, physical_len)
        || physical.len() - pad_positions.len() != logical.len()
    {
        return false;
    }
    let mut start = 0;
    let mut logical_start = 0;
    for &pad in pad_positions.iter().chain([&physical_len]) {
        let run = &physical[start..pad as usize];
        if logical[logical_start..logical_start + run.len()] != *run {
            return false;
        }
        logical_start += run.len();
        start = pad as usize + 1;
    }
    true
}

/// Write the pad positions inside the physical interval
/// `[slice_start, slice_start + slice_len)` to `out_pads`, relative to
/// `slice_start`, and return how many were written.
///
/// The slice's logical origin is `logical_boundary(slice_start, ..)`.
/// Returns `None` for invalid pad positions, an interval beyond
/// `physical_len`, or an `out_pads` shorter than the pads in the interval.
///
/// ```
/// # use cache_reuse_utils::slice_pad_positions;
/// let mut out = [0; 4];
/// assert_eq!(slice_pad_positions(&[1, 3, 6], 8, 2, 4, &mut out), Some(1));
/// assert_eq!(out[0], 1);
/// ```
pub fn slice_pad_positions(
    pad_positions: &[u32],
    physical_len: u32,
    slice_start: u32,
    slice_len: u32,
    out_pads: &mut [u32],
) -> Option<u32> {
    let slice_end = slice_start.checked_add(slice_len)?;
    if slice_end > physical_len || !validate_pad_positions(pad_positions, physical_len) {
        return None;
    }
    let first = pad_positions.partition_point(|&pad| pad < slice_start);
    let last = pad_positions.partition_point(|&pad| pad < slice_end);
    let kept = &pad_positions[first..last];
    for (out, &pad) in out_pads.get_mut(..kept.len())?.iter_mut().zip(kept) {
        *out = pad - slice_start;
    }
    Some(kept.len() as u32)
}

/// Physical length after appending `appended_tokens` logical tokens to a
/// sequence of `physical_len` tokens. Appended tokens are never pads, so the
/// pad positions are unchanged.
///
/// Returns `None` for invalid pad positions or a length beyond `u32`.
///
/// ```
/// # use cache_reuse_utils::append_unpadded;
/// assert_eq!(append_unpadded(5, &[1, 3], 2), Some(7));
/// assert_eq!(append_unpadded(u32::MAX, &[], 1), None);
/// ```
pub fn append_unpadded(
    physical_len: u32,
    pad_positions: &[u32],
    appended_tokens: u32,
) -> Option<u32> {
    if !validate_pad_positions(pad_positions, physical_len) {
        return None;
    }
    physical_len.checked_add(appended_tokens)
}

/// Number of leading `pad_positions` kept when a physical sequence of
/// `physical_len` tokens is truncated to `new_physical_len`: those strictly
/// below the new length.
///
/// Returns `None` for invalid pad positions or `new_physical_len >
/// physical_len`.
///
/// ```
/// # use cache_reuse_utils::truncate_pad_positions;
/// assert_eq!(truncate_pad_positions(&[1, 3], 5, 3), Some(1));
/// assert_eq!(truncate_pad_positions(&[1, 3], 5, 6), None);
/// ```
pub fn truncate_pad_positions(
    pad_positions: &[u32],
    physical_len: u32,
    new_physical_len: u32,
) -> Option<usize> {
    if new_physical_len > physical_len || !validate_pad_positions(pad_positions, physical_len) {
        return None;
    }
    Some(pad_positions.partition_point(|&pad| pad < new_physical_len))
}

/// Whether a reused range copies its source pads to the same places in the
/// destination.
///
/// Compares the source pads in `[range.source_start, range.source_end())`
/// with the destination pads in `[range.prepared_start,
/// range.prepared_end())`, the destination's physical coordinates. They
/// must have the same offsets relative to their range starts, and no pad may
/// sit on the first or last token of the range: a range starts and ends on
/// logical tokens, so its edges are unambiguous.
///
/// False for invalid pad positions, an empty range, or a range beyond either
/// length. Runs in `O(log P + pads in range)`.
///
/// ```
/// # use cache_reuse_utils::{PlacedRange, verify_span_correspondence};
/// let range = PlacedRange { source_start: 2, destination_start: 0, token_count: 4, prepared_start: 10 };
/// assert!(verify_span_correspondence(&[4], 8, &[12], 16, range));
/// assert!(!verify_span_correspondence(&[4], 8, &[13], 16, range));
/// ```
pub fn verify_span_correspondence(
    source_pads: &[u32],
    source_len: u32,
    dst_pads: &[u32],
    dst_len: u32,
    range: PlacedRange,
) -> bool {
    let count = range.token_count;
    let in_bounds = |start: u32, len: u32| start.checked_add(count).is_some_and(|end| end <= len);
    if count == 0
        || !in_bounds(range.source_start, source_len)
        || !in_bounds(range.prepared_start, dst_len)
        || !validate_pad_positions(source_pads, source_len)
        || !validate_pad_positions(dst_pads, dst_len)
    {
        return false;
    }
    fn offsets(pads: &[u32], start: u32, count: u32) -> impl Iterator<Item = u32> + '_ {
        let first = pads.partition_point(|&pad| pad < start);
        let last = pads.partition_point(|&pad| pad < start + count);
        pads[first..last].iter().map(move |&pad| pad - start)
    }
    let mut source = offsets(source_pads, range.source_start, count);
    let mut destination = offsets(dst_pads, range.prepared_start, count);
    loop {
        match (source.next(), destination.next()) {
            (None, None) => return true,
            (Some(a), Some(b)) if a == b && a != 0 && a + 1 != count => {}
            _ => return false,
        }
    }
}
