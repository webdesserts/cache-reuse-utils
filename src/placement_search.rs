//! Placement search over discovered maximal matches.
//!
//! Search proceeds in four phases:
//!
//! 1. **Variant derivation:** Emit each maximal match untrimmed first, followed
//!    by any extra trims requested by `max_trim_left` and `max_trim_right`,
//!    up to `max_variants`, sorted by ascending `destination_end`.
//! 2. **Greedy incumbent seeding:** Greedily chain variants from longest to
//!    shortest while coverage improves, so an early deadline or state limit
//!    still returns a plan built from the longest matches.
//! 3. **Arena dynamic programming:** Extend partial plans (`Label`s in a flat
//!    parent-pointer arena) in `destination_end` order. Each step left-trims
//!    the next variant just enough to clear the previous range and satisfy
//!    source/count alignment, then evaluates both the minimally padded
//!    placement and the zero-pad alignment left-trimmed alternative. Non-overlapping
//!    predecessors are queried in `O(log² V)` via [`index::Index`], while
//!    overlapping predecessors are visited from ordered end-coordinate maps.
//! 4. **Plan selection:** Among completed plans within the policy's
//!    [`crate::LossAllowance`] of the best raw coverage, choose the plan that
//!    maximizes `covered - padding * pad_penalty_tokens -
//!    (ranges - 1) * range_penalty_tokens`.
//!
//! Two exact prunings keep high `k` cheap. Labels for one variant with the
//! same padding and ends keep only those that trade more ranges for strictly
//! more coverage. A label whose reachable coverage (see [`bound`]) is below
//! the loss floor of the best coverage so far is dropped.

use std::cmp::Reverse;
use std::collections::HashMap;

use crate::candidate::CandidateRange;
use crate::error::PlanErrorKind;
use crate::placement::{Insertion, PlacedRange, PlacementPolicy, PlacementStatus};
use crate::timing::{Stop, Stopper};

mod bound;
mod index;

use bound::Bound;
use index::{Entry, Index};

/// Cap on retained plan labels. A label is 48 bytes, so labels take at most
/// 12 MiB; their index entries add a few times that in the worst case. At
/// `k = 32` a full search over 4,096 short repeated matches needs more than
/// 2^16 labels, so the cap is 2^18.
pub(crate) const MAX_STATES: usize = 1 << 18;

const NO_PARENT: u32 = u32::MAX;

/// Bound on re-trims when source alignment keeps moving a piece past an
/// aligned prepared start. Exceeding it drops only the trimmed alternative.
const MAX_ALIGNMENT_RETRIES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Variant {
    source_start: u32,
    destination_start: u32,
    token_count: u32,
}

impl Variant {
    fn destination_end(self) -> u32 {
        self.destination_start + self.token_count
    }

    fn source_end(self) -> u32 {
        self.source_start + self.token_count
    }
}

/// One step of a plan: the placed piece plus the totals of the plan that ends
/// with it. `parent` indexes the arena.
#[derive(Debug, Clone, Copy)]
struct Label {
    parent: u32,
    piece: Variant,
    /// Insertion made before this piece: `(boundary, count)`, count may be 0.
    insertion: (u32, u32),
    /// The piece directly continues the parent's range and shares its slot.
    merged: bool,
    padding: u32,
    ranges: u32,
    covered: u64,
}

/// Search result: the selected plan, or a deliberate empty plan.
#[derive(Debug)]
pub(crate) struct PlacementOutcome {
    pub plan: Vec<PlacedRange>,
    pub insertions: Vec<Insertion>,
    pub covered: u64,
    pub padding: u32,
    pub best_useful: u64,
    pub status: PlacementStatus,
}

impl PlacementOutcome {
    pub(crate) fn empty(best_useful: u64, status: PlacementStatus) -> Self {
        Self {
            plan: Vec::new(),
            insertions: Vec::new(),
            covered: 0,
            padding: 0,
            best_useful,
            status,
        }
    }
}

/// Untrimmed matches first, then the declared trim rectangle, up to
/// `max_variants`. Distinct maximal matches never share a sub-range, so no
/// deduplication is needed. Variants no trim can make usable are skipped.
/// Returns the variants in destination-end order and whether the cap truncated
/// them.
fn derive_variants(
    matches: &[CandidateRange],
    policy: &PlacementPolicy<'_>,
    stopper: &mut Stopper<'_>,
) -> Result<(Vec<Variant>, bool), Stop> {
    let mut variants = Vec::with_capacity(matches.len().min(policy.max_variants));
    let mut truncated = false;
    'outer: for trimmed in [false, true] {
        for m in matches {
            let max_left = if trimmed { policy.max_trim_left } else { 0 };
            let max_right = if trimmed { policy.max_trim_right } else { 0 };
            for left in 0..=max_left.min(m.token_count - 1) {
                for right in 0..=max_right.min(m.token_count - 1) {
                    stopper.tick()?;
                    if trimmed == (left == 0 && right == 0) || left + right >= m.token_count {
                        continue;
                    }
                    let variant = Variant {
                        source_start: m.source_start + left,
                        destination_start: m.destination_start + left,
                        token_count: m.token_count - left - right,
                    };
                    if fit(variant, None, policy).is_none() {
                        continue;
                    }
                    if variants.len() == policy.max_variants {
                        truncated = true;
                        break 'outer;
                    }
                    variants.push(variant);
                }
            }
        }
    }
    // A placed piece can only follow pieces that end before it, and left
    // trims keep a variant's end, so end order visits every predecessor first.
    // A match contained in a longer one's span therefore precedes the longer
    // one's trimmed remainder.
    variants.sort_unstable_by_key(|v| (v.destination_end(), v.destination_start, v.source_start));
    Ok((variants, truncated))
}

/// Left-trim `variant` to start at or after `after` (a previous range's
/// destination and source ends), then to source-start and count alignment.
fn fit(
    variant: Variant,
    after: Option<(u32, u32)>,
    policy: &PlacementPolicy<'_>,
) -> Option<Variant> {
    let overlap = after.map_or(0, |(destination, source)| {
        destination
            .saturating_sub(variant.destination_start)
            .max(source.saturating_sub(variant.source_start))
    });
    let source_start = (variant.source_start as u64 + overlap as u64)
        .next_multiple_of(policy.source_start_alignment as u64);
    let trim = u32::try_from(source_start - variant.source_start as u64).ok()?;
    let mut count = variant.token_count.checked_sub(trim)?;
    count -= count % policy.reuse_count_alignment;
    if count == 0 || count < policy.minimum_physical_reuse_tokens {
        return None;
    }
    Some(Variant {
        source_start: source_start as u32,
        destination_start: variant.destination_start + trim,
        token_count: count,
    })
}

/// Smallest padding that meets prepared-start alignment, the minimum prepared
/// start, the remaining pad budget and prepared capacity.
fn minimal_padding(
    base_prepared: u64,
    prior_padding: u32,
    policy: &PlacementPolicy<'_>,
    context_len: usize,
) -> Option<u32> {
    let padding = u32::try_from(alignment_gap(base_prepared, policy)).ok()?;
    let total = prior_padding.checked_add(padding)?;
    let prepared = context_len as u64 + total as u64;
    (total <= policy.max_new_padding && prepared <= policy.max_prepared_tokens as u64)
        .then_some(padding)
}

/// Tokens needed before `base_prepared` to meet prepared-start alignment and
/// the minimum prepared start, ignoring budgets.
fn alignment_gap(base_prepared: u64, policy: &PlacementPolicy<'_>) -> u64 {
    let alignment = policy.prepared_target_start_alignment as u64;
    base_prepared
        .max(policy.minimum_prepared_reuse_start as u64)
        .next_multiple_of(alignment)
        - base_prepared
}

struct Search<'a> {
    context_len: usize,
    policy: &'a PlacementPolicy<'a>,
    k: u32,
    labels: Vec<Label>,
    max_states: usize,
    stopped_for_deadline: bool,
    limit_status: Option<PlacementStatus>,
}

impl Search<'_> {
    /// Place `variant` after `parent`, or first when `parent` is `None`.
    ///
    /// When the fitted piece needs padding, there are two placements: the
    /// padded one, and the piece left-trimmed until its prepared start meets
    /// alignment and the minimum with no padding. Either may be infeasible.
    fn step(&self, parent: Option<u32>, variant: Variant) -> [Option<Label>; 2] {
        let previous = parent.map(|index| self.labels[index as usize]);
        let after = previous.map(|label| (label.piece.destination_end(), label.piece.source_end()));
        let Some(piece) = fit(variant, after, self.policy) else {
            return [None, None];
        };
        let prior_padding = previous.map_or(0, |label| label.padding);
        let base_prepared = piece.destination_start as u64 + prior_padding as u64;
        match minimal_padding(base_prepared, prior_padding, self.policy, self.context_len) {
            Some(0) => [self.place(parent, previous, piece, 0), None],
            pad => [
                pad.and_then(|pad| self.place(parent, previous, piece, pad)),
                self.trim_to_aligned(variant, piece, after, prior_padding)
                    .and_then(|piece| self.place(parent, previous, piece, 0)),
            ],
        }
    }

    /// Left-trim `piece` (fitted from `variant`) until its prepared start
    /// needs no padding. Source alignment can move the start past an aligned
    /// prepared start, so this retries a bounded number of times.
    fn trim_to_aligned(
        &self,
        variant: Variant,
        mut piece: Variant,
        after: Option<(u32, u32)>,
        prior_padding: u32,
    ) -> Option<Variant> {
        let source_floor = after.map_or(0, |(_, source)| source);
        for _ in 0..MAX_ALIGNMENT_RETRIES {
            let base = piece.destination_start as u64 + prior_padding as u64;
            match alignment_gap(base, self.policy) {
                0 => return Some(piece),
                gap => {
                    let destination = u32::try_from(piece.destination_start as u64 + gap).ok()?;
                    piece = fit(variant, Some((destination, source_floor)), self.policy)?;
                }
            }
        }
        None
    }

    /// The label for `piece` after `previous`, with `pad` tokens inserted at
    /// the latest allowed boundary before it, if the policy permits.
    fn place(
        &self,
        parent: Option<u32>,
        previous: Option<Label>,
        piece: Variant,
        pad: u32,
    ) -> Option<Label> {
        let suffix_start = self.context_len as u32 - self.policy.fresh_suffix_tokens;
        if piece.destination_start < self.policy.protected_prefix_tokens
            || piece.destination_end() > suffix_start
        {
            return None;
        }
        let after = previous.map(|label| (label.piece.destination_end(), label.piece.source_end()));
        let gap_start = after.map_or(self.policy.protected_prefix_tokens, |(end, _)| end);
        let boundary = if pad == 0 {
            piece.destination_start
        } else {
            self.policy
                .allowed_insertions
                .latest_in(gap_start, piece.destination_start)?
        };
        let merged = pad == 0 && after == Some((piece.destination_start, piece.source_start));
        let ranges = previous.map_or(0, |label| label.ranges) + u32::from(!merged);
        if ranges > self.k {
            return None;
        }
        Some(Label {
            parent: parent.unwrap_or(NO_PARENT),
            piece,
            insertion: (boundary, pad),
            merged,
            padding: previous.map_or(0, |label| label.padding) + pad,
            ranges,
            covered: previous.map_or(0, |label| label.covered) + piece.token_count as u64,
        })
    }

    /// Coverage net of the pad and range penalties, the first selection
    /// criterion. The first range is free. Every term is far below `i64`
    /// limits: coverage is under 2^20 and each penalty product under 2^45.
    fn net(&self, label: &Label) -> i64 {
        let pads = label.padding as i64 * self.policy.pad_penalty_tokens as i64;
        let splits =
            label.ranges.saturating_sub(1) as i64 * self.policy.range_penalty_tokens as i64;
        label.covered as i64 - pads - splits
    }

    /// Labels for `chain` (variant indexes in destination-end order), appended
    /// after the arena's current end, or `None` if the chain is infeasible.
    fn chain(&mut self, variants: &[Variant], chain: &[usize]) -> Option<u64> {
        let base = self.labels.len();
        let mut parent = None;
        for &index in chain {
            let [padded, trimmed] = self.step(parent, variants[index]);
            let best = match (padded, trimmed) {
                (Some(padded), Some(trimmed)) if self.net(&trimmed) >= self.net(&padded) => {
                    Some(trimmed)
                }
                (padded, trimmed) => padded.or(trimmed),
            };
            match best {
                Some(label) => {
                    parent = Some(self.labels.len() as u32);
                    self.labels.push(label);
                }
                None => {
                    self.labels.truncate(base);
                    return None;
                }
            }
        }
        parent.map(|index| self.labels[index as usize].covered)
    }

    /// Greedy incumbent: add variants longest first while coverage improves.
    /// A stop keeps the chain built so far.
    fn seed_greedy(&mut self, variants: &[Variant], stopper: &mut Stopper<'_>) -> Result<(), Stop> {
        let mut order: Vec<usize> = (0..variants.len()).collect();
        order.sort_unstable_by_key(|&i| (Reverse(variants[i].token_count), i));
        let mut chosen: Vec<usize> = Vec::new();
        let mut best = 0;
        let mut stop = Ok(());
        for index in order {
            stop = stopper.tick();
            if stop.is_err() {
                break;
            }
            let mut candidate = chosen.clone();
            candidate.insert(chosen.partition_point(|&c| c < index), index);
            let base = self.labels.len();
            let covered = self.chain(variants, &candidate);
            self.labels.truncate(base);
            if covered.is_some_and(|covered| covered > best) {
                best = covered.unwrap_or(0);
                chosen = candidate;
            }
        }
        self.chain(variants, &chosen);
        stop
    }

    /// Index labels from `start` on. Stops the search when the index is full.
    fn index_from(&mut self, index: &mut Index, start: usize) {
        for (label, entry) in self.labels.iter().enumerate().skip(start) {
            let added = index.insert(Entry {
                label: label as u32,
                destination_end: entry.piece.destination_end(),
                source_end: entry.piece.source_end(),
                ranges: entry.ranges,
                padding: entry.padding,
                covered: entry.covered,
            });
            if !added {
                self.limit_status = Some(PlacementStatus::StateLimitReached);
                return;
            }
        }
    }

    /// Offer `variant` after every label that can precede it: the best clear
    /// predecessor per `(ranges, padding)` group, labels ending exactly at its
    /// start (which may coalesce), and every overlapping label.
    fn extend(
        &self,
        index: &Index,
        variant: Variant,
        stopper: &mut Stopper<'_>,
        offer: &mut impl FnMut(Label),
    ) -> Result<(), Stop> {
        let mut try_parent = |parent: u32| {
            for label in self.step(Some(parent), variant).into_iter().flatten() {
                offer(label);
            }
        };
        let Some(start) = fit(variant, None, self.policy) else {
            return Ok(());
        };
        let (destination, source) = (start.destination_start, start.source_start);
        for (group, &(ranges, padding)) in index.groups().iter().enumerate() {
            stopper.tick()?;
            if ranges >= self.k {
                continue;
            }
            // A clear predecessor ends at or before the destination start.
            // A padded placement also needs an allowed boundary after it; the
            // trimmed alternative does not.
            let base = destination as u64 + padding as u64;
            let padded_end = match minimal_padding(base, padding, self.policy, self.context_len) {
                Some(0) => None,
                Some(_) => self.policy.allowed_insertions.latest_in(0, destination),
                None => None,
            };
            let best = index.best_clear(group, destination, source);
            if let Some(parent) = best {
                try_parent(parent);
            }
            if let Some(parent) = padded_end
                .filter(|&end| end < destination)
                .and_then(|end| index.best_clear(group, end, source))
                .filter(|&parent| Some(parent) != best)
            {
                try_parent(parent);
            }
        }
        for parent in index.ending_in_destination(destination..destination + 1) {
            stopper.tick()?;
            if self.labels[parent as usize].piece.source_end() == source {
                try_parent(parent);
            }
        }
        for parent in index.ending_in_destination(destination + 1..variant.destination_end()) {
            stopper.tick()?;
            if self.labels[parent as usize].piece.source_end() < variant.source_end() {
                try_parent(parent);
            }
        }
        for parent in index.ending_in_source(source + 1..variant.source_end()) {
            stopper.tick()?;
            if self.labels[parent as usize].piece.destination_end() <= destination {
                try_parent(parent);
            }
        }
        Ok(())
    }

    fn run(&mut self, variants: &[Variant], stopper: &mut Stopper<'_>) -> Result<(), Stop> {
        let starts = variants
            .iter()
            .filter_map(|&variant| fit(variant, None, self.policy))
            .map(|start| start.source_start)
            .collect();
        let mut index = Index::new(starts);
        let seeded = self.seed_greedy(variants, stopper);
        self.index_from(&mut index, 0);
        seeded?;
        let window_end = self.context_len as u32 - self.policy.fresh_suffix_tokens;
        let ends = variants
            .iter()
            .map(|v| (v.destination_end(), v.token_count));
        let bound = Bound::new(ends, self.k, window_end);
        let mut best = self
            .labels
            .iter()
            .map(|label| label.covered)
            .max()
            .unwrap_or(0);
        // (padding, destination end, source end) -> slots in `fresh`. A label
        // with no more ranges and at least the coverage of another with the
        // same key extends to at least as good a plan, so each key keeps only
        // labels that trade more ranges for strictly more coverage.
        let mut slots: HashMap<(u32, u32, u32), Vec<usize>> = HashMap::new();
        let mut fresh: Vec<Option<Label>> = Vec::new();
        for &variant in variants {
            if self.limit_status == Some(PlacementStatus::StateLimitReached) {
                return Ok(());
            }
            slots.clear();
            let floor = self.policy.loss_allowance.floor(best);
            let mut offer = |label: Label| {
                let end = label.piece.destination_end();
                if bound.reachable(end, label.ranges, label.covered) < floor {
                    return;
                }
                let key = (
                    label.padding,
                    label.piece.destination_end(),
                    label.piece.source_end(),
                );
                let front = slots.entry(key).or_default();
                let kept = |slot: &usize| fresh[*slot].expect("front slots are live");
                if front
                    .iter()
                    .map(kept)
                    .any(|other| other.ranges <= label.ranges && other.covered >= label.covered)
                {
                    return;
                }
                front.retain(|&slot| {
                    let other = fresh[slot].expect("front slots are live");
                    let dominated = other.ranges >= label.ranges && other.covered <= label.covered;
                    if dominated {
                        fresh[slot] = None;
                    }
                    !dominated
                });
                front.push(fresh.len());
                fresh.push(Some(label));
            };
            let mut stop = stopper.tick().err();
            if stop.is_none() {
                for label in self.step(None, variant).into_iter().flatten() {
                    offer(label);
                }
                stop = self.extend(&index, variant, stopper, &mut offer).err();
            }
            let room = self.max_states.saturating_sub(self.labels.len());
            let first_new = self.labels.len();
            let live = fresh.drain(..).flatten();
            self.labels.extend(live.take(room.saturating_add(1)));
            if self.labels.len() > first_new + room {
                self.labels.truncate(first_new + room);
                self.limit_status = Some(PlacementStatus::StateLimitReached);
            }
            for label in &self.labels[first_new..] {
                best = best.max(label.covered);
            }
            self.index_from(&mut index, first_new);
            match stop {
                Some(Stop::Cancelled) => return Err(Stop::Cancelled),
                Some(Stop::Deadline) => {
                    self.stopped_for_deadline = true;
                    return Ok(());
                }
                None if self.limit_status == Some(PlacementStatus::StateLimitReached) => {
                    return Ok(());
                }
                None => {}
            }
        }
        Ok(())
    }

    /// The plan ending at `index`, with insertions and coalesced ranges.
    fn reconstruct(&self, index: usize) -> (Vec<PlacedRange>, Vec<Insertion>) {
        let mut steps = Vec::new();
        let mut cursor = index as u32;
        while cursor != NO_PARENT {
            let label = self.labels[cursor as usize];
            steps.push(label);
            cursor = label.parent;
        }
        let mut plan: Vec<PlacedRange> = Vec::new();
        let mut insertions = Vec::new();
        for label in steps.into_iter().rev() {
            let (boundary, count) = label.insertion;
            if count > 0 {
                insertions.push(Insertion {
                    before_target_index: boundary,
                    token: self.policy.pad_token.unwrap_or_default(),
                    count,
                });
            }
            match plan.last_mut() {
                Some(last) if label.merged => last.token_count += label.piece.token_count,
                _ => plan.push(PlacedRange {
                    source_start: label.piece.source_start,
                    destination_start: label.piece.destination_start,
                    token_count: label.piece.token_count,
                    prepared_start: label.piece.destination_start + label.padding,
                }),
            }
        }
        (plan, insertions)
    }

    /// Pick the plan to return. Eligible plans cover at least the loss floor
    /// of the best coverage. Among them, prefer the most coverage net of the
    /// pad and range penalties, then less padding, more coverage and fewer
    /// ranges.
    fn select(self) -> Result<PlacementOutcome, PlanErrorKind> {
        let status = if self.stopped_for_deadline {
            PlacementStatus::DeadlinePartial
        } else {
            self.limit_status.unwrap_or(PlacementStatus::Complete)
        };
        let Some(best_useful) = self.labels.iter().map(|label| label.covered).max() else {
            return match (self.stopped_for_deadline, self.limit_status) {
                (true, _) => Err(PlanErrorKind::BudgetExhausted),
                (false, Some(status)) => Err(PlanErrorKind::PlacementIncomplete(status)),
                (false, None) => Ok(PlacementOutcome::empty(0, PlacementStatus::NoFeasibleReuse)),
            };
        };
        let admissible_min = self.policy.loss_allowance.floor(best_useful);
        let rank = |label: &Label| {
            (
                self.net(label),
                Reverse(label.padding),
                label.covered,
                Reverse(label.ranges),
            )
        };
        let mut selected: Option<usize> = None;
        for (index, label) in self.labels.iter().enumerate() {
            if label.covered >= admissible_min
                && selected.is_none_or(|best| rank(label) > rank(&self.labels[best]))
            {
                selected = Some(index);
            }
        }
        let index = selected.ok_or(PlanErrorKind::Invariant("best plan is not eligible"))?;
        let label = self.labels[index];
        if admissible_min == 0 && self.net(&label) <= 0 {
            return Ok(PlacementOutcome::empty(best_useful, status));
        }
        let (plan, insertions) = self.reconstruct(index);
        Ok(PlacementOutcome {
            plan,
            insertions,
            covered: label.covered,
            padding: label.padding,
            best_useful,
            status,
        })
    }
}

/// Run the placement search over discovered maximal matches.
pub(crate) fn search(
    context_len: usize,
    matches: &[CandidateRange],
    k: u32,
    policy: &PlacementPolicy<'_>,
    stopper: &mut Stopper<'_>,
) -> Result<PlacementOutcome, PlanErrorKind> {
    search_with_cap(context_len, matches, k, policy, stopper, MAX_STATES)
}

fn search_with_cap(
    context_len: usize,
    matches: &[CandidateRange],
    k: u32,
    policy: &PlacementPolicy<'_>,
    stopper: &mut Stopper<'_>,
    max_states: usize,
) -> Result<PlacementOutcome, PlanErrorKind> {
    let mut search = Search {
        context_len,
        policy,
        k,
        labels: Vec::new(),
        max_states,
        stopped_for_deadline: false,
        limit_status: None,
    };
    match derive_variants(matches, policy, stopper) {
        Ok((variants, truncated)) => {
            if truncated {
                search.limit_status = Some(PlacementStatus::VariantLimitReached);
            }
            match search.run(&variants, stopper) {
                Ok(()) => {}
                Err(Stop::Cancelled) => return Err(PlanErrorKind::Cancelled),
                Err(Stop::Deadline) => search.stopped_for_deadline = true,
            }
        }
        Err(Stop::Cancelled) => return Err(PlanErrorKind::Cancelled),
        Err(Stop::Deadline) => search.stopped_for_deadline = true,
    }
    if stopper.cancelled() {
        return Err(PlanErrorKind::Cancelled);
    }
    search.select()
}

#[cfg(test)]
mod tests;
