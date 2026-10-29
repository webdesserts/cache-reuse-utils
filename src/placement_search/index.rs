//! 2D predecessor index for placement search.
//!
//! When extending a partial plan (`Label`) with a candidate `Variant`, a prior
//! label relates to the variant in one of two ways:
//!
//! - **Clear (non-overlapping) predecessor:** The label ends at or before the
//!   variant's aligned start in both target and source coordinates. Because a
//!   clear predecessor does not force `fit()` to trim the variant, every clear
//!   predecessor in the same `(ranges, padding)` group produces the exact same
//!   placed piece. Only the predecessor with the highest `covered` total in
//!   that 2D prefix region matters. [`Index::best_clear`] answers that query
//!   (`destination_end <= max_end` and `source_end <= source_bound`) in
//!   `O(log² V)` time per group using a Fenwick tree over variant source starts
//!   whose nodes each maintain a Pareto staircase of `(destination_end, covered)`.
//! - **Overlapping or adjacent predecessor:** The label ends inside the
//!   variant's target or source span, so `fit()` must left-trim the variant by
//!   an amount that depends on the predecessor's exact end position. Those
//!   predecessors are stored in `BTreeMap`s keyed by `destination_end` and
//!   `source_end` so only labels ending inside the variant's span are visited.

use std::collections::{BTreeMap, HashMap};

/// Cap on Fenwick nodes across all `(ranges, padding)` groups. Each node is a
/// small vector header, so this bounds the index at about 50 MiB even with
/// many padding values.
pub(super) const MAX_INDEX_NODES: usize = 1 << 21;

/// Pareto front of `(destination_end, covered, label)`: destination ends
/// ascending and coverage strictly ascending, so the last entry at or before
/// a bound has the best coverage.
type Staircase = Vec<(u32, u64, u32)>;

fn staircase_insert(entries: &mut Staircase, end: u32, covered: u64, label: u32) {
    // Labels mostly arrive in destination order: append without searching.
    match entries.last() {
        None => return entries.push((end, covered, label)),
        Some(&(last_end, last_covered, _)) if last_end < end => {
            if last_covered < covered {
                entries.push((end, covered, label));
            }
            return;
        }
        Some(_) => {}
    }
    let mut at = entries.partition_point(|entry| entry.0 <= end);
    if at > 0 && entries[at - 1].1 >= covered {
        return;
    }
    if at > 0 && entries[at - 1].0 == end {
        at -= 1;
    }
    let dominated = entries[at..]
        .iter()
        .take_while(|entry| entry.0 == end || entry.1 <= covered)
        .count();
    entries.splice(at..at + dominated, [(end, covered, label)]);
}

fn staircase_best(entries: &Staircase, max_end: u32) -> Option<(u64, u32)> {
    if let Some(&(end, covered, label)) = entries.last()
        && end <= max_end
    {
        return Some((covered, label));
    }
    let at = entries.partition_point(|entry| entry.0 <= max_end);
    at.checked_sub(1).map(|i| (entries[i].1, entries[i].2))
}

/// The ends and totals of a label, as the index sees it.
#[derive(Debug, Clone, Copy)]
pub(super) struct Entry {
    pub label: u32,
    pub destination_end: u32,
    pub source_end: u32,
    pub ranges: u32,
    pub padding: u32,
    pub covered: u64,
}

pub(super) struct Index {
    /// Sorted, unique aligned source starts of all variants: the only source
    /// bounds ever queried.
    axis: Vec<u32>,
    groups: HashMap<(u32, u32), usize>,
    keys: Vec<(u32, u32)>,
    trees: Vec<Vec<Staircase>>,
    /// Labels by end position. Labels from one variant mostly share an end,
    /// so each key holds a short list.
    by_destination: BTreeMap<u32, Vec<u32>>,
    by_source: BTreeMap<u32, Vec<u32>>,
}

impl Index {
    pub(super) fn new(mut axis: Vec<u32>) -> Self {
        axis.sort_unstable();
        axis.dedup();
        Self {
            axis,
            groups: HashMap::new(),
            keys: Vec::new(),
            trees: Vec::new(),
            by_destination: BTreeMap::new(),
            by_source: BTreeMap::new(),
        }
    }

    /// The `(ranges, padding)` groups seen so far.
    pub(super) fn groups(&self) -> &[(u32, u32)] {
        &self.keys
    }

    /// Add a label. Returns `false` when a new group would exceed
    /// [`MAX_INDEX_NODES`]; the label is then not indexed.
    pub(super) fn insert(&mut self, entry: Entry) -> bool {
        let key = (entry.ranges, entry.padding);
        let group = match self.groups.get(&key) {
            Some(&group) => group,
            None => {
                let size = self.axis.len() + 1;
                if (self.trees.len() + 1) * size > MAX_INDEX_NODES {
                    return false;
                }
                self.trees.push(vec![Staircase::new(); size]);
                self.keys.push(key);
                self.groups.insert(key, self.keys.len() - 1);
                self.keys.len() - 1
            }
        };
        self.by_destination
            .entry(entry.destination_end)
            .or_default()
            .push(entry.label);
        self.by_source
            .entry(entry.source_end)
            .or_default()
            .push(entry.label);
        // Position of the first queryable bound at or after the source end.
        let mut node = self.axis.partition_point(|&start| start < entry.source_end) + 1;
        let tree = &mut self.trees[group];
        while node < tree.len() {
            staircase_insert(
                &mut tree[node],
                entry.destination_end,
                entry.covered,
                entry.label,
            );
            node += node & node.wrapping_neg();
        }
        true
    }

    /// The best-covered label in `group` with `destination_end <= max_end`
    /// and `source_end <= source_bound`. `source_bound` must be on the axis.
    pub(super) fn best_clear(&self, group: usize, max_end: u32, source_bound: u32) -> Option<u32> {
        let tree = &self.trees[group];
        let mut node = self.axis.partition_point(|&start| start <= source_bound);
        let mut best: Option<(u64, u32)> = None;
        while node > 0 {
            if let Some(found) = staircase_best(&tree[node], max_end)
                && best.is_none_or(|current| found.0 > current.0)
            {
                best = Some(found);
            }
            node -= node & node.wrapping_neg();
        }
        best.map(|(_, label)| label)
    }

    /// Labels whose destination end is in `range`.
    pub(super) fn ending_in_destination(
        &self,
        range: std::ops::Range<u32>,
    ) -> impl Iterator<Item = u32> + '_ {
        self.by_destination
            .range(range)
            .flat_map(|(_, labels)| labels.iter().copied())
    }

    /// Labels whose source end is in `range`.
    pub(super) fn ending_in_source(
        &self,
        range: std::ops::Range<u32>,
    ) -> impl Iterator<Item = u32> + '_ {
        self.by_source
            .range(range)
            .flat_map(|(_, labels)| labels.iter().copied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staircase_keeps_only_the_pareto_front() {
        let mut steps = Staircase::new();
        staircase_insert(&mut steps, 10, 5, 0);
        staircase_insert(&mut steps, 20, 4, 1); // dominated by (10, 5)
        staircase_insert(&mut steps, 30, 9, 2);
        staircase_insert(&mut steps, 5, 6, 3); // dominates (10, 5)
        staircase_insert(&mut steps, 30, 10, 4); // replaces (30, 9)
        assert_eq!(steps, [(5, 6, 3), (30, 10, 4)]);
        assert_eq!(staircase_best(&steps, 4), None);
        assert_eq!(staircase_best(&steps, 29), Some((6, 3)));
        assert_eq!(staircase_best(&steps, 30), Some((10, 4)));
    }

    #[test]
    fn best_clear_respects_both_bounds() {
        let mut index = Index::new(vec![0, 10, 20]);
        let entry = |label, destination_end, source_end, covered| Entry {
            label,
            destination_end,
            source_end,
            ranges: 1,
            padding: 0,
            covered,
        };
        assert!(index.insert(entry(0, 5, 5, 3)));
        assert!(index.insert(entry(1, 15, 8, 7)));
        assert!(index.insert(entry(2, 8, 18, 9)));
        assert_eq!(index.best_clear(0, 20, 10), Some(1));
        assert_eq!(index.best_clear(0, 10, 10), Some(0));
        assert_eq!(index.best_clear(0, 10, 20), Some(2));
        assert_eq!(index.best_clear(0, 4, 20), None);
    }
}
