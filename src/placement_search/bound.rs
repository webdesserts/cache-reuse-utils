//! Upper bound on the coverage a partial plan can still reach.
//!
//! Every piece placed after a label ends later in the target, so it comes
//! from a variant later in destination-end order. Pieces that continue one
//! range all lie in one maximal match, and a variant's token count is at most
//! its match's length. A label with `r` ranges can therefore gain at most the
//! `k - r + 1` longest variants that end after it: one for its current range
//! and one per free slot. Unplaced target tokens bound the gain too.
//!
//! The search drops a label whose bound is below the loss floor of the best
//! coverage found so far. Neither it nor any extension could be eligible,
//! because the final floor is at least as high.

/// Prefix sums of the longest variant lengths for every suffix of the
/// variants in destination-end order.
pub(super) struct Bound {
    ends: Vec<u32>,
    /// `(ends.len() + 1) * (k + 1)` sums: row `i` holds, for `m` in `0..=k`,
    /// the total of the `m` longest variants from index `i` on.
    top: Vec<u64>,
    k: usize,
    window_end: u32,
}

impl Bound {
    /// `variants` are `(destination_end, token_count)` in destination-end
    /// order.
    pub(super) fn new(variants: impl Iterator<Item = (u32, u32)>, k: u32, window_end: u32) -> Self {
        let k = k as usize;
        let variants: Vec<(u32, u32)> = variants.collect();
        let mut top = vec![0; (variants.len() + 1) * (k + 1)];
        // The k longest lengths seen so far, longest first.
        let mut longest: Vec<u32> = Vec::with_capacity(k + 1);
        for (i, &(_, count)) in variants.iter().enumerate().rev() {
            let at = longest.partition_point(|&other| other >= count);
            if at < k {
                longest.insert(at, count);
                longest.truncate(k);
            }
            let row = &mut top[i * (k + 1)..(i + 1) * (k + 1)];
            for m in 1..=k {
                row[m] = row[m - 1] + longest.get(m - 1).copied().unwrap_or(0) as u64;
            }
        }
        Self {
            ends: variants.iter().map(|&(end, _)| end).collect(),
            top,
            k,
            window_end,
        }
    }

    /// Most coverage a plan ending at `destination_end` with `ranges` ranges
    /// and `covered` tokens could still reach.
    pub(super) fn reachable(&self, destination_end: u32, ranges: u32, covered: u64) -> u64 {
        let first = self.ends.partition_point(|&end| end <= destination_end);
        let pieces = (self.k + 1).saturating_sub(ranges as usize).min(self.k);
        let longest = self.top[first * (self.k + 1) + pieces];
        let unplaced = self.window_end.saturating_sub(destination_end) as u64;
        covered + longest.min(unplaced)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bound_takes_the_longest_later_variants_and_unplaced_tokens() {
        // Variants ending at 10, 20, 30, 40 with lengths 5, 9, 2, 7.
        let bound = Bound::new([(10, 5), (20, 9), (30, 2), (40, 7)].into_iter(), 2, 100);
        // After 10, two ranges left: one current range plus one slot, so
        // the two longest later variants (9 + 7).
        assert_eq!(bound.reachable(10, 1, 3), 3 + 16);
        // A full plan can still continue its current range: one variant.
        assert_eq!(bound.reachable(10, 2, 3), 3 + 9);
        // Near the window end the unplaced tokens bind.
        assert_eq!(bound.reachable(35, 1, 3), 3 + 7);
        let tight = Bound::new([(10, 5), (40, 30)].into_iter(), 2, 45);
        assert_eq!(tight.reachable(35, 1, 0), 10);
    }
}
