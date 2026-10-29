//! Placement search over discovered maximal matches.
//!
//! 1. Derive variants: each maximal match, then the declared trim rectangle,
//!    capped at `max_variants`.
//! 2. Seed a greedy incumbent so a deadline still returns a global plan.
//! 3. Extend plans in destination order. Each step left-trims the next
//!    variant just enough to clear the previous range and meet source and
//!    count alignment, then inserts the minimal padding the policy requires.
//!    Plans live in an arena of parent-pointer labels, one per
//!    `(variant, end, ranges, padding)` with the best coverage.
//! 4. Select among plans within the loss allowance of the best coverage,
//!    charging `pad_penalty_tokens` per inserted pad.

use std::cmp::Reverse;
use std::collections::HashMap;

use crate::candidate::CandidateRange;
use crate::error::PlanErrorKind;
use crate::placement::{Insertion, PlacedRange, PlacementPolicy, PlacementStatus};
use crate::timing::{Stop, Stopper};

/// Cap on retained plan labels. A label is a few dozen bytes, so this bounds
/// search memory to a few MiB.
pub(crate) const MAX_STATES: usize = 1 << 16;

const NO_PARENT: u32 = u32::MAX;

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
/// Returns the variants in destination order and whether the cap truncated
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
    variants.sort_unstable_by_key(|v| (v.destination_start, v.source_start, v.token_count));
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
    let alignment = policy.prepared_target_start_alignment as u64;
    let start = base_prepared
        .max(policy.minimum_prepared_reuse_start as u64)
        .next_multiple_of(alignment);
    let padding = u32::try_from(start - base_prepared).ok()?;
    let total = prior_padding.checked_add(padding)?;
    let prepared = context_len as u64 + total as u64;
    (total <= policy.max_new_padding && prepared <= policy.max_prepared_tokens as u64)
        .then_some(padding)
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
    fn step(&self, parent: Option<u32>, variant: Variant) -> Option<Label> {
        let previous = parent.map(|index| self.labels[index as usize]);
        let after = previous.map(|label| (label.piece.destination_end(), label.piece.source_end()));
        let piece = fit(variant, after, self.policy)?;
        let suffix_start = self.context_len as u32 - self.policy.fresh_suffix_tokens;
        if piece.destination_start < self.policy.protected_prefix_tokens
            || piece.destination_end() > suffix_start
        {
            return None;
        }
        let gap_start = after.map_or(self.policy.protected_prefix_tokens, |(end, _)| end);
        let prior_padding = previous.map_or(0, |label| label.padding);
        let base_prepared = piece.destination_start as u64 + prior_padding as u64;
        let pad = minimal_padding(base_prepared, prior_padding, self.policy, self.context_len)?;
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
            padding: prior_padding + pad,
            ranges,
            covered: previous.map_or(0, |label| label.covered) + piece.token_count as u64,
        })
    }

    /// Labels for `chain` (variant indexes in destination order), appended
    /// after the arena's current end, or `None` if the chain is infeasible.
    fn chain(&mut self, variants: &[Variant], chain: &[usize]) -> Option<u64> {
        let base = self.labels.len();
        let mut parent = None;
        for &index in chain {
            match self.step(parent, variants[index]) {
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

    fn run(&mut self, variants: &[Variant], stopper: &mut Stopper<'_>) -> Result<(), Stop> {
        self.seed_greedy(variants, stopper)?;
        // (ranges, padding, destination end, source end) -> index in `fresh`.
        let mut slots: HashMap<(u32, u32, u32, u32), usize> = HashMap::new();
        let mut fresh: Vec<Label> = Vec::new();
        for &variant in variants {
            slots.clear();
            let mut offer = |label: Label| {
                let key = (
                    label.ranges,
                    label.padding,
                    label.piece.destination_end(),
                    label.piece.source_end(),
                );
                match slots.get(&key) {
                    Some(&slot) if fresh[slot].covered >= label.covered => {}
                    Some(&slot) => fresh[slot] = label,
                    None => {
                        slots.insert(key, fresh.len());
                        fresh.push(label);
                    }
                }
            };
            let mut stop = stopper.tick().err();
            if stop.is_none() {
                if let Some(label) = self.step(None, variant) {
                    offer(label);
                }
                for parent in 0..self.labels.len() {
                    if let Err(error) = stopper.tick() {
                        stop = Some(error);
                        break;
                    }
                    let piece = self.labels[parent].piece;
                    if piece.destination_end() >= variant.destination_end()
                        || piece.source_end() >= variant.source_end()
                    {
                        continue;
                    }
                    if let Some(label) = self.step(Some(parent as u32), variant) {
                        offer(label);
                    }
                }
            }
            let room = self.max_states.saturating_sub(self.labels.len());
            if fresh.len() > room {
                fresh.truncate(room);
                self.limit_status = Some(PlacementStatus::StateLimitReached);
            }
            self.labels.append(&mut fresh);
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
    /// pad penalty, then less padding, more coverage and fewer ranges.
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
        let penalty = |label: &Label| label.padding as u64 * self.policy.pad_penalty_tokens as u64;
        let rank = |label: &Label| {
            (
                label.covered.saturating_sub(penalty(label)),
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
        if admissible_min == 0 && label.covered <= penalty(&label) {
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
