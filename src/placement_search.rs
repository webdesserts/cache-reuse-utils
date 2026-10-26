//! Bounded placement-aware search over actual-token maximal matches.
//!
//! Discovery supplies maximal exact matches. This module derives the declared
//! trim variants, applies the canonical insertion rule for each fresh gap, and
//! searches over continuation states. It is the only search pipeline used by
//! [`crate::CacheReuseAnalysis`].

use std::collections::{HashMap, HashSet};
use std::mem::size_of;

use crate::candidate::{CandidateRange, MAX_CANDIDATES, MAX_RANGES};
use crate::placement::{
    Insertion, MAX_PLACEMENT_BYTES, PlacedRange, PlacementPolicy, PlacementStatus,
};
use crate::timing::{CheckOutcome, DeadlineClock};

/// Retained headroom for publishing a fully computed completed plan before
/// optional extension-state storage consumes the rest of the model.
///
/// The maximum modeled bucket payload is:
///
/// `(MAX_NEW_PADDING + 1) * size_of::<Option<Partial>>()`
/// `+ (MAX_NEW_PADDING + 1) * (size_of::<Partial>()
///    + K * (size_of::<PlacedRange>() + size_of::<Insertion>()))`
///
/// because `copy_partial` gives bucket plans exact capacities and a plan has at
/// most K ranges/insertion groups. A private unit assertion checks this bound
/// against this 4 MiB reserve. Allocator, `HashMap` table and other spare
/// capacity are excluded and are not claimed as RSS.
const PUBLICATION_RESERVE_BYTES: u64 = 4 * 1024 * 1024;

/// Internal search failure. The caller-facing wrapper adds stage/timing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchError {
    Cancelled,
    BudgetExhausted,
    AdmissionDenied,
    LimitReached(PlacementStatus),
    Invariant(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Variant {
    source_start: u32,
    destination_start: u32,
    token_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct StateKey {
    last: PlacedRange,
    ranges: u32,
    padding: u32,
}

#[derive(Debug, Clone)]
struct Partial {
    plan: Vec<PlacedRange>,
    insertions: Vec<Insertion>,
    covered: u64,
    padding: u32,
    last_variant: usize,
}

impl Partial {
    fn last(&self) -> PlacedRange {
        *self.plan.last().expect("partial plans are non-empty")
    }
}

fn plan_less(a: &Partial, b: &Partial) -> bool {
    for (left, right) in a.plan.iter().zip(&b.plan) {
        let left_key = (
            left.destination_start,
            left.source_start,
            left.token_count,
            left.prepared_start,
        );
        let right_key = (
            right.destination_start,
            right.source_start,
            right.token_count,
            right.prepared_start,
        );
        if left_key != right_key {
            return left_key < right_key;
        }
    }
    if a.plan.len() != b.plan.len() {
        return a.plan.len() < b.plan.len();
    }
    for (left, right) in a.insertions.iter().zip(&b.insertions) {
        let left_key = (left.before_target_index, left.token, left.count);
        let right_key = (right.before_target_index, right.token, right.count);
        if left_key != right_key {
            return left_key < right_key;
        }
    }
    a.insertions.len() < b.insertions.len()
}

fn partial_better(candidate: &Partial, incumbent: &Partial) -> bool {
    candidate.covered > incumbent.covered
        || (candidate.covered == incumbent.covered
            && (candidate.plan.len() < incumbent.plan.len()
                || (candidate.plan.len() == incumbent.plan.len()
                    && plan_less(candidate, incumbent))))
}

fn copy_partial(partial: &Partial) -> Result<Partial, SearchError> {
    let mut plan = Vec::new();
    plan.try_reserve_exact(partial.plan.len())
        .map_err(|_| SearchError::AdmissionDenied)?;
    plan.extend_from_slice(&partial.plan);
    let mut insertions = Vec::new();
    insertions
        .try_reserve_exact(partial.insertions.len())
        .map_err(|_| SearchError::AdmissionDenied)?;
    insertions.extend_from_slice(&partial.insertions);
    Ok(Partial {
        plan,
        insertions,
        covered: partial.covered,
        padding: partial.padding,
        last_variant: partial.last_variant,
    })
}

/// Derive variants from discovered maximal matches using the declared trim
/// rectangle. The nested scan checks cancellation/deadline at every iteration,
/// including rejected variants, and uses fallible dedupe/vector reservation.
/// Returns whether the hard/default variant budget truncated the declared set.
fn derive_variants(
    matches: &[CandidateRange],
    policy: &PlacementPolicy<'_>,
    clock: &mut DeadlineClock<'_>,
) -> Result<(Vec<Variant>, bool), SearchError> {
    let mut variants = Vec::new();
    let mut seen = HashSet::new();
    let mut truncated = false;
    'outer: for range in matches {
        match clock.check() {
            CheckOutcome::Continue => {}
            CheckOutcome::DeadlineReached => return Err(SearchError::BudgetExhausted),
            CheckOutcome::Cancelled => return Err(SearchError::Cancelled),
        }
        if range.token_count == 0 {
            continue;
        }
        let max_left = policy.max_trim_left.min(range.token_count - 1);
        let max_right = policy.max_trim_right.min(range.token_count - 1);
        for left in 0..=max_left {
            for right in 0..=max_right {
                match clock.check() {
                    CheckOutcome::Continue => {}
                    CheckOutcome::DeadlineReached => {
                        return Err(SearchError::BudgetExhausted);
                    }
                    CheckOutcome::Cancelled => return Err(SearchError::Cancelled),
                }
                let Some(count) = range.token_count.checked_sub(left + right) else {
                    continue;
                };
                if count == 0 {
                    continue;
                }
                let source_start = range.source_start + left;
                let destination_start = range.destination_start + left;
                if source_start % policy.source_start_alignment != 0
                    || count % policy.reuse_count_alignment != 0
                    || count < policy.minimum_physical_reuse_tokens
                {
                    continue;
                }
                let variant = Variant {
                    source_start,
                    destination_start,
                    token_count: count,
                };
                seen.try_reserve(1)
                    .map_err(|_| SearchError::AdmissionDenied)?;
                if seen.insert(variant) {
                    if variants.len() >= policy.max_variants {
                        truncated = true;
                        break 'outer;
                    }
                    variants
                        .try_reserve(1)
                        .map_err(|_| SearchError::AdmissionDenied)?;
                    variants.push(variant);
                }
            }
        }
    }
    variants.sort_unstable_by(|a, b| {
        (a.destination_start, a.source_start, a.token_count).cmp(&(
            b.destination_start,
            b.source_start,
            b.token_count,
        ))
    });
    variants.dedup();
    if variants.len() > policy.max_variants {
        variants.truncate(policy.max_variants);
        truncated = true;
    }
    Ok((variants, truncated))
}

fn checked_end(start: u32, count: u32) -> Option<u32> {
    start.checked_add(count)
}

/// Smallest insertion count satisfying target alignment, minimum prepared
/// start, capacity and total pad budget.
fn minimal_padding(
    base_prepared: u64,
    prior_padding: u32,
    policy: &PlacementPolicy<'_>,
    max_new_padding: u32,
    context_len: usize,
) -> Option<u32> {
    let alignment = policy.prepared_target_start_alignment as u64;
    let mut padding = if alignment <= 1 {
        0
    } else {
        (alignment - base_prepared % alignment) % alignment
    };
    let minimum = policy.minimum_prepared_reuse_start as u64;
    if base_prepared + padding < minimum {
        let need = minimum - (base_prepared + padding);
        let add = need.div_ceil(alignment) * alignment;
        padding = padding.checked_add(add)?;
    }
    let padding32 = u32::try_from(padding).ok()?;
    if padding32 > max_new_padding {
        return None;
    }
    let total_prepared = (context_len as u64)
        .checked_add(prior_padding as u64)?
        .checked_add(padding32 as u64)?;
    if total_prepared > policy.max_prepared_tokens as u64 {
        return None;
    }
    Some(padding32)
}

fn latest_allowed_boundary(
    policy: &PlacementPolicy<'_>,
    gap_start: u32,
    destination_start: u32,
) -> Option<u32> {
    let end = policy
        .allowed_insertions
        .partition_point(|&value| value <= destination_start);
    let boundary = *policy.allowed_insertions.get(end.checked_sub(1)?)?;
    (boundary >= gap_start).then_some(boundary)
}

fn try_push_plan(plan: &mut Vec<PlacedRange>, value: PlacedRange) -> Result<(), SearchError> {
    plan.try_reserve(1)
        .map_err(|_| SearchError::AdmissionDenied)?;
    plan.push(value);
    Ok(())
}

fn try_push_insertion(
    insertions: &mut Vec<Insertion>,
    value: Insertion,
) -> Result<(), SearchError> {
    insertions
        .try_reserve(1)
        .map_err(|_| SearchError::AdmissionDenied)?;
    insertions.push(value);
    Ok(())
}

/// Search result, including a selected plan or a deliberate empty plan.
#[derive(Debug)]
pub(crate) struct PlacementOutcome {
    pub plan: Vec<PlacedRange>,
    pub insertions: Vec<Insertion>,
    pub covered: u64,
    pub padding: u32,
    pub best_useful: u64,
    pub status: PlacementStatus,
}

struct Search<'a> {
    context_len: usize,
    policy: &'a PlacementPolicy<'a>,
    k: u32,
    variants: Vec<Variant>,
    states: HashMap<StateKey, Partial>,
    order: Vec<StateKey>,
    buckets: Vec<Option<Partial>>,
    modeled_bytes: u64,
    bucket_bytes: u64,
    state_limit: u64,
    best_useful: u64,
    stopped_for_deadline: bool,
    limit_status: Option<PlacementStatus>,
}

impl<'a> Search<'a> {
    fn new(
        context_len: usize,
        policy: &'a PlacementPolicy<'a>,
        k: u32,
        variants: Vec<Variant>,
    ) -> Result<Self, SearchError> {
        let bucket_len = policy.max_new_padding as usize + 1;
        let mut buckets = Vec::new();
        buckets
            .try_reserve_exact(bucket_len)
            .map_err(|_| SearchError::AdmissionDenied)?;
        buckets.resize_with(bucket_len, || None);
        let variants_bytes = variants
            .capacity()
            .checked_mul(size_of::<Variant>())
            .ok_or(SearchError::AdmissionDenied)? as u64;
        let bucket_bytes = buckets
            .capacity()
            .checked_mul(size_of::<Option<Partial>>())
            .ok_or(SearchError::AdmissionDenied)? as u64;
        let modeled_bytes = variants_bytes
            .checked_add(bucket_bytes)
            .ok_or(SearchError::AdmissionDenied)?;
        let state_limit = MAX_PLACEMENT_BYTES.saturating_sub(PUBLICATION_RESERVE_BYTES);
        if modeled_bytes > MAX_PLACEMENT_BYTES || variants_bytes > state_limit {
            return Err(SearchError::LimitReached(
                PlacementStatus::StateLimitReached,
            ));
        }
        Ok(Self {
            context_len,
            policy,
            k,
            variants,
            states: HashMap::new(),
            order: Vec::new(),
            buckets,
            modeled_bytes,
            bucket_bytes,
            state_limit,
            best_useful: 0,
            stopped_for_deadline: false,
            limit_status: None,
        })
    }

    #[cfg(test)]
    fn with_state_limit(mut self, state_limit: u64) -> Self {
        self.state_limit = state_limit;
        self
    }

    #[cfg(test)]
    fn new_with_state_limit(
        context_len: usize,
        policy: &'a PlacementPolicy<'a>,
        k: u32,
        variants: Vec<Variant>,
        state_limit: u64,
    ) -> Result<Self, SearchError> {
        let search = Self::new(context_len, policy, k, variants)?.with_state_limit(state_limit);
        if search.modeled_bytes.saturating_sub(search.bucket_bytes) > state_limit {
            return Err(SearchError::LimitReached(
                PlacementStatus::StateLimitReached,
            ));
        }
        Ok(search)
    }

    fn state_bytes(_key: &StateKey, partial: &Partial) -> Option<u64> {
        Some(
            (size_of::<StateKey>()
                + size_of::<Partial>()
                + partial
                    .plan
                    .capacity()
                    .checked_mul(size_of::<PlacedRange>())?
                + partial
                    .insertions
                    .capacity()
                    .checked_mul(size_of::<Insertion>())?) as u64
                + size_of::<StateKey>() as u64,
        )
    }

    fn partial_payload(partial: &Partial) -> Option<u64> {
        Some(
            (size_of::<Partial>()
                + partial
                    .plan
                    .capacity()
                    .checked_mul(size_of::<PlacedRange>())?
                + partial
                    .insertions
                    .capacity()
                    .checked_mul(size_of::<Insertion>())?) as u64,
        )
    }

    fn account_insert(&mut self, key: &StateKey, partial: &Partial) -> Result<(), SearchError> {
        let bytes = Self::state_bytes(key, partial).ok_or(SearchError::AdmissionDenied)?;
        let state_payload = self
            .modeled_bytes
            .checked_sub(self.bucket_bytes)
            .ok_or(SearchError::Invariant("bucket accounting exceeds model"))?;
        let next_state = state_payload
            .checked_add(bytes)
            .ok_or(SearchError::AdmissionDenied)?;
        if next_state > self.state_limit {
            self.limit_status = Some(PlacementStatus::StateLimitReached);
            return Err(SearchError::LimitReached(
                PlacementStatus::StateLimitReached,
            ));
        }
        self.modeled_bytes = self
            .modeled_bytes
            .checked_add(bytes)
            .ok_or(SearchError::AdmissionDenied)?;
        Ok(())
    }

    fn insert_state(&mut self, key: StateKey, partial: Partial) -> Result<(), SearchError> {
        if let Some(existing) = self.states.get(&key) {
            if partial_better(&partial, existing) {
                let old_bytes =
                    Self::state_bytes(&key, existing).ok_or(SearchError::AdmissionDenied)?;
                let new_bytes =
                    Self::state_bytes(&key, &partial).ok_or(SearchError::AdmissionDenied)?;
                let state_payload = self
                    .modeled_bytes
                    .checked_sub(self.bucket_bytes)
                    .ok_or(SearchError::Invariant("bucket accounting exceeds model"))?;
                let modeled_state = state_payload
                    .checked_sub(old_bytes)
                    .and_then(|value| value.checked_add(new_bytes))
                    .ok_or(SearchError::AdmissionDenied)?;
                if modeled_state > self.state_limit {
                    self.limit_status = Some(PlacementStatus::StateLimitReached);
                    return Err(SearchError::LimitReached(
                        PlacementStatus::StateLimitReached,
                    ));
                }
                self.modeled_bytes = self
                    .bucket_bytes
                    .checked_add(modeled_state)
                    .ok_or(SearchError::AdmissionDenied)?;
                self.states.insert(key, partial);
            }
            return Ok(());
        }
        self.account_insert(&key, &partial)?;
        self.states
            .try_reserve(1)
            .map_err(|_| SearchError::AdmissionDenied)?;
        self.order
            .try_reserve(1)
            .map_err(|_| SearchError::AdmissionDenied)?;
        self.states.insert(key, partial);
        self.order.push(key);
        Ok(())
    }

    fn update_bucket(&mut self, partial: &Partial) -> Result<(), SearchError> {
        let index = partial.padding as usize;
        let Some(bucket) = self.buckets.get_mut(index) else {
            return Err(SearchError::Invariant("bucket index out of range"));
        };
        let replace = match bucket {
            None => true,
            Some(existing) => partial_better(partial, existing),
        };
        if replace {
            let copy = copy_partial(partial)?;
            let old_bytes = bucket.as_ref().and_then(Self::partial_payload).unwrap_or(0);
            let new_bytes = Self::partial_payload(&copy).ok_or(SearchError::AdmissionDenied)?;
            let modeled = self
                .modeled_bytes
                .saturating_sub(old_bytes)
                .checked_add(new_bytes)
                .ok_or(SearchError::AdmissionDenied)?;
            if modeled > MAX_PLACEMENT_BYTES {
                self.limit_status = Some(PlacementStatus::StateLimitReached);
                return Err(SearchError::LimitReached(
                    PlacementStatus::StateLimitReached,
                ));
            }
            self.bucket_bytes = self
                .bucket_bytes
                .saturating_sub(old_bytes)
                .checked_add(new_bytes)
                .ok_or(SearchError::AdmissionDenied)?;
            self.modeled_bytes = modeled;
            self.best_useful = self.best_useful.max(copy.covered);
            *bucket = Some(copy);
        }
        Ok(())
    }

    /// Build the successor partial for one current variant from an optional
    /// predecessor. `None` starts a plan at the protected prefix.
    fn transition(
        &self,
        predecessor: Option<&Partial>,
        variant: &Variant,
        last_variant: usize,
    ) -> Result<Option<Partial>, SearchError> {
        let destination_end = checked_end(variant.destination_start, variant.token_count)
            .ok_or(SearchError::Invariant("variant destination overflows"))?;
        let source_end = checked_end(variant.source_start, variant.token_count)
            .ok_or(SearchError::Invariant("variant source overflows"))?;
        let suffix_start = (self.context_len as u64)
            .checked_sub(self.policy.fresh_suffix_tokens as u64)
            .ok_or(SearchError::Invariant("fresh suffix exceeds target"))?;
        if variant.destination_start < self.policy.protected_prefix_tokens
            || destination_end as u64 > suffix_start
        {
            return Ok(None);
        }

        let (gap_start, prior_padding, base_plan_len, last_range) = match predecessor {
            None => (self.policy.protected_prefix_tokens, 0u32, 0usize, None),
            Some(partial) => {
                let last = partial.last();
                if last.destination_end() > variant.destination_start
                    || last.source_end() > variant.source_start
                {
                    return Ok(None);
                }
                (
                    last.destination_end(),
                    partial.padding,
                    partial.plan.len(),
                    Some(last),
                )
            }
        };

        let base_prepared = variant.destination_start as u64 + prior_padding as u64;
        let Some(padding) = minimal_padding(
            base_prepared,
            prior_padding,
            self.policy,
            self.policy.max_new_padding.saturating_sub(prior_padding),
            self.context_len,
        ) else {
            return Ok(None);
        };
        let prepared_start = base_prepared + padding as u64;
        if prepared_start > u32::MAX as u64 {
            return Ok(None);
        }
        let prepared_start = prepared_start as u32;

        let mut boundary = None;
        if padding > 0 {
            boundary = latest_allowed_boundary(self.policy, gap_start, variant.destination_start);
            if boundary.is_none() {
                return Ok(None);
            }
        }

        let adjacent = last_range.is_some_and(|last| {
            last.destination_end() == variant.destination_start
                && last.source_end() == variant.source_start
                && padding == 0
        });

        let mut partial = match predecessor {
            Some(partial) => {
                let mut copy = copy_partial(partial)?;
                copy.plan
                    .try_reserve(1)
                    .map_err(|_| SearchError::AdmissionDenied)?;
                copy.insertions
                    .try_reserve(1)
                    .map_err(|_| SearchError::AdmissionDenied)?;
                copy
            }
            None => Partial {
                plan: Vec::new(),
                insertions: Vec::new(),
                covered: 0,
                padding: 0,
                last_variant,
            },
        };

        if let Some(boundary) = boundary {
            try_push_insertion(
                &mut partial.insertions,
                Insertion {
                    before_target_index: boundary,
                    token: self.policy.pad_token.expect("validated pad token"),
                    count: padding,
                },
            )?;
        }

        if adjacent {
            if let Some(last) = partial.plan.last_mut() {
                last.token_count = last
                    .token_count
                    .checked_add(variant.token_count)
                    .ok_or(SearchError::Invariant("coalesced count overflows"))?;
            }
        } else {
            if partial.plan.len() >= self.k as usize {
                return Ok(None);
            }
            let prepared_end = prepared_start
                .checked_add(variant.token_count)
                .ok_or(SearchError::Invariant("prepared range overflows"))?;
            let _ = prepared_end;
            try_push_plan(
                &mut partial.plan,
                PlacedRange {
                    source_start: variant.source_start,
                    destination_start: variant.destination_start,
                    token_count: variant.token_count,
                    prepared_start,
                },
            )?;
            if !adjacent {
                // A new range is allowed only when its predecessor (if any) is
                // strictly non-adjacent. The `adjacent` branch above handled
                // the merge case.
            }
        }
        if partial.plan.len() > self.k as usize {
            return Ok(None);
        }
        partial.covered = partial
            .covered
            .checked_add(variant.token_count as u64)
            .ok_or(SearchError::Invariant("coverage overflows"))?;
        partial.padding = prior_padding + padding;
        partial.last_variant = last_variant;
        let _ = (source_end, base_plan_len);
        Ok(Some(partial))
    }

    /// Publish a fully computed candidate before optional extension storage.
    /// If modeled extension storage reaches its limit, the completed bucket
    /// remains available; actual allocator admission errors propagate unchanged.
    fn publish_and_store(&mut self, partial: &Partial) -> Result<(), SearchError> {
        match self.update_bucket(partial) {
            Ok(()) => {}
            Err(SearchError::LimitReached(status)) => {
                self.limit_status = Some(status);
                return Err(SearchError::LimitReached(status));
            }
            // Allocation failure is not a modeled limit. Propagate it as the
            // typed admission error; only the modeled cap becomes a limit status.
            Err(error) => return Err(error),
        }
        let key = StateKey {
            last: partial.last(),
            ranges: partial.plan.len() as u32,
            padding: partial.padding,
        };
        let stored = copy_partial(partial)?;
        match self.insert_state(key, stored) {
            Ok(()) => Ok(()),
            Err(SearchError::LimitReached(status)) => {
                self.limit_status = Some(status);
                Err(SearchError::LimitReached(status))
            }
            Err(error) => Err(error),
        }
    }

    fn run(mut self, clock: &mut DeadlineClock<'_>) -> Result<PlacementOutcome, SearchError> {
        let mut completed = true;
        'outer: for j in 0..self.variants.len() {
            match clock.check() {
                CheckOutcome::Continue => {}
                CheckOutcome::DeadlineReached => {
                    self.stopped_for_deadline = true;
                    completed = false;
                    break 'outer;
                }
                CheckOutcome::Cancelled => return Err(SearchError::Cancelled),
            }
            let variant = self.variants[j];
            let start = self.transition(None, &variant, j)?;
            if let Some(partial) = start {
                match self.publish_and_store(&partial) {
                    Ok(()) => {}
                    Err(SearchError::LimitReached(status)) => {
                        self.limit_status = Some(status);
                        break 'outer;
                    }
                    Err(error) => return Err(error),
                }
            }
            let snapshot = self.order.len();
            for index in 0..snapshot {
                // Every natural state iteration is a cancellation/deadline
                // point. There is no modulo or work-counter scheduling.
                match clock.check() {
                    CheckOutcome::Continue => {}
                    CheckOutcome::DeadlineReached => {
                        self.stopped_for_deadline = true;
                        completed = false;
                        break 'outer;
                    }
                    CheckOutcome::Cancelled => return Err(SearchError::Cancelled),
                }
                let key = self.order[index];
                let Some(partial) = self.states.get(&key).cloned() else {
                    continue;
                };
                if partial.last_variant == j {
                    continue;
                }
                if let Some(next) = self.transition(Some(&partial), &variant, j)? {
                    match self.publish_and_store(&next) {
                        Ok(()) => {}
                        Err(SearchError::LimitReached(status)) => {
                            self.limit_status = Some(status);
                            break 'outer;
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
        }
        self.finish(clock, completed)
    }

    fn finish(
        mut self,
        clock: &mut DeadlineClock<'_>,
        completed: bool,
    ) -> Result<PlacementOutcome, SearchError> {
        match clock.check() {
            CheckOutcome::Cancelled => return Err(SearchError::Cancelled),
            CheckOutcome::DeadlineReached if !completed => self.stopped_for_deadline = true,
            CheckOutcome::DeadlineReached | CheckOutcome::Continue => {}
        }
        let best_useful = self.best_useful;
        if best_useful == 0 {
            if self.stopped_for_deadline {
                return Err(SearchError::BudgetExhausted);
            }
            if let Some(status) = self.limit_status {
                return Err(SearchError::LimitReached(status));
            }
            return Ok(PlacementOutcome {
                plan: Vec::new(),
                insertions: Vec::new(),
                covered: 0,
                padding: 0,
                best_useful: 0,
                status: PlacementStatus::NoFeasibleReuse,
            });
        }
        let allowance = self.policy.loss_allowance.basis_points() as u64;
        let loss = best_useful.saturating_mul(allowance) / 10_000;
        let admissible_min = best_useful - loss;

        let mut selected: Option<&Partial> = None;
        for bucket in &self.buckets {
            let Some(candidate) = bucket else {
                continue;
            };
            if candidate.covered < admissible_min {
                continue;
            }
            let replace = match selected {
                None => true,
                Some(existing) => {
                    candidate.padding < existing.padding
                        || (candidate.padding == existing.padding
                            && partial_better(candidate, existing))
                }
            };
            if replace {
                selected = Some(candidate);
            }
        }
        // At 100% allowance, a zero-covered empty plan competes after B is
        // known. It is not seeded before a valid plan exists.
        if admissible_min == 0 && selected.is_none_or(|p| p.padding > 0) {
            return Ok(PlacementOutcome {
                plan: Vec::new(),
                insertions: Vec::new(),
                covered: 0,
                padding: 0,
                best_useful,
                status: if self.stopped_for_deadline {
                    PlacementStatus::DeadlinePartial
                } else if let Some(status) = self.limit_status {
                    status
                } else {
                    PlacementStatus::Complete
                },
            });
        }
        let selected = selected.ok_or(SearchError::Invariant(
            "best completed plan has no admissible bucket",
        ))?;
        let status = if self.stopped_for_deadline {
            PlacementStatus::DeadlinePartial
        } else if let Some(status) = self.limit_status {
            status
        } else {
            PlacementStatus::Complete
        };
        Ok(PlacementOutcome {
            plan: selected.plan.clone(),
            insertions: selected.insertions.clone(),
            covered: selected.covered,
            padding: selected.padding,
            best_useful,
            status,
        })
    }
}

/// Run the placement-aware search. `matches` are maximal logical matches from
/// discovery; they are not fed through the raw candidate-pool validator.
pub(crate) fn search(
    context_len: usize,
    matches: &[CandidateRange],
    k: u32,
    policy: &PlacementPolicy<'_>,
    clock: &mut DeadlineClock<'_>,
) -> Result<PlacementOutcome, SearchError> {
    if context_len > u32::MAX as usize {
        return Err(SearchError::Invariant("target exceeds u32 coordinates"));
    }
    let (variants, truncated) = derive_variants(matches, policy, clock)?;
    let mut search = Search::new(context_len, policy, k, variants)?;
    if truncated {
        search.limit_status = Some(PlacementStatus::VariantLimitReached);
    }
    search.run(clock)
}

/// Compatibility helper for the raw candidate-pool model. It does not model the
/// placement state payload and is not used to admit placement variants.
pub fn scratch_bytes(count: usize, k: u32) -> Option<u64> {
    if k == 0 || k > MAX_RANGES || count > MAX_CANDIDATES {
        return None;
    }
    if count == 0 {
        return Some(0);
    }
    // Retained raw-pool model: one 16-byte state per (candidate, range count).
    let states = (count as u64).checked_mul((k as u64 + 1).checked_mul(16)?)?;
    let tables = (count as u64).checked_mul((size_of::<usize>() * 2) as u64)?;
    let plan = 3u64
        .checked_mul(k as u64)?
        .checked_mul(size_of::<CandidateRange>() as u64)?;
    states.checked_add(tables)?.checked_add(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate::CandidateRange;
    use crate::placement::LossAllowance;
    use crate::timing::DeadlineClock;
    use std::time::{Duration, Instant};

    fn range(source_start: u32, destination_start: u32, token_count: u32) -> CandidateRange {
        CandidateRange {
            source_start,
            destination_start,
            token_count,
        }
    }

    fn clock<'a>(deadline: Instant, cancel: &'a mut dyn FnMut() -> bool) -> DeadlineClock<'a> {
        let base = Instant::now();
        DeadlineClock::with_clock(base, deadline, move || base, cancel)
    }

    fn run_at(
        context_len: usize,
        matches: &[CandidateRange],
        k: u32,
        policy: &PlacementPolicy<'_>,
    ) -> PlacementOutcome {
        let mut cancel = || false;
        let mut clock = clock(Instant::now() + Duration::from_secs(5), &mut cancel);
        search(context_len, matches, k, policy, &mut clock).expect("search")
    }

    fn run(matches: &[CandidateRange], k: u32, policy: &PlacementPolicy<'_>) -> PlacementOutcome {
        run_at(20, matches, k, policy)
    }

    fn no_pad<'a>(context_len: usize) -> PlacementPolicy<'a> {
        PlacementPolicy::default_for(context_len)
    }

    #[test]
    fn publication_reserve_covers_the_modeled_max_bucket_payload() {
        // This mirrors the admission model: bucket table element storage plus
        // one exact-capacity Partial/plan/insertion payload per padding bucket.
        // It is deliberately a conservative upper bound (the table slot and
        // payload header are both charged). The insertion representation is
        // three u32 fields; use size_of rather than assuming a source-level
        // byte count.
        assert_eq!(size_of::<Insertion>(), 3 * size_of::<u32>());
        let buckets = crate::placement::MAX_NEW_PADDING as u64 + 1;
        let table = buckets * size_of::<Option<Partial>>() as u64;
        let per_bucket = size_of::<Partial>() as u64
            + MAX_RANGES as u64 * (size_of::<PlacedRange>() + size_of::<Insertion>()) as u64;
        let max_bucket_payload = table + buckets * per_bucket;
        assert!(
            max_bucket_payload <= PUBLICATION_RESERVE_BYTES,
            "modeled max bucket payload {max_bucket_payload} exceeds reserve {PUBLICATION_RESERVE_BYTES}"
        );
    }

    #[test]
    fn no_pad_controls_keep_10_vs_6_and_coalesced_10() {
        let policy = no_pad(20);
        let first = [range(0, 0, 4), range(0, 0, 5), range(10, 10, 6)];
        let plan = run(&first, 2, &policy);
        assert_eq!(plan.covered, 11);
        let second = [range(0, 0, 4), range(0, 0, 5), range(4, 4, 6)];
        let plan = run(&second, 2, &policy);
        assert_eq!(plan.covered, 10);
        assert_eq!(plan.plan.len(), 1);
    }

    fn limited_outcome(
        context_len: usize,
        matches: &[CandidateRange],
        k: u32,
        policy: &PlacementPolicy<'_>,
        state_limit: u64,
    ) -> Result<PlacementOutcome, SearchError> {
        let base = Instant::now();
        let mut cancel = || false;
        let mut clock = clock(base + Duration::from_secs(5), &mut cancel);
        let (variants, truncated) = derive_variants(matches, policy, &mut clock)?;
        let mut search = Search::new(context_len, policy, k, variants)?;
        if truncated {
            search.limit_status = Some(PlacementStatus::VariantLimitReached);
        }
        search = search.with_state_limit(state_limit);
        search.run(&mut clock)
    }

    #[test]
    fn first_completed_plan_survives_state_cap_and_floor_is_correct() {
        let policy = PlacementPolicy::default_for(20);
        let matches = [range(0, 0, 4)];
        let base = Instant::now();
        let mut cancel = || false;
        let mut clock = clock(base + Duration::from_secs(5), &mut cancel);
        let (variants, _) = derive_variants(&matches, &policy, &mut clock).expect("variants");
        let search = Search::new(20, &policy, 1, variants).expect("search");
        let base_state = search.modeled_bytes - search.bucket_bytes;
        drop(search);
        let outcome = limited_outcome(20, &matches, 1, &policy, base_state).expect("published");
        assert_eq!(outcome.covered, 4);
        assert_eq!(outcome.best_useful, 4);
        assert_eq!(outcome.status, PlacementStatus::StateLimitReached);
    }

    #[test]
    fn later_best_plan_survives_state_cap() {
        let policy = PlacementPolicy::default_for(20);
        let matches = [range(0, 0, 2), range(10, 10, 6)];
        let base = Instant::now();
        let mut cancel = || false;
        let mut clock = clock(base + Duration::from_secs(5), &mut cancel);
        let (variants, _) = derive_variants(&matches, &policy, &mut clock).expect("variants");
        let search = Search::new(20, &policy, 1, variants).expect("search");
        let base_state = search.modeled_bytes - search.bucket_bytes;
        let first = search
            .transition(None, &search.variants[0], 0)
            .expect("transition")
            .expect("first");
        let first_bytes = Search::state_bytes(
            &StateKey {
                last: first.last(),
                ranges: first.plan.len() as u32,
                padding: first.padding,
            },
            &first,
        )
        .expect("bytes");
        drop(search);
        let outcome = limited_outcome(20, &matches, 1, &policy, base_state + first_bytes + 1)
            .expect("published later");
        assert_eq!(outcome.covered, 6);
        assert_eq!(outcome.best_useful, 6);
        assert_eq!(outcome.status, PlacementStatus::StateLimitReached);
    }

    #[test]
    fn state_cap_without_a_plan_returns_a_limiter_error() {
        let policy = PlacementPolicy::default_for(20);
        let matches = [range(0, 0, 4)];
        let base = Instant::now();
        let mut cancel = || false;
        let mut clock = clock(base + Duration::from_secs(5), &mut cancel);
        let (variants, _) = derive_variants(&matches, &policy, &mut clock).expect("variants");
        assert!(matches!(
            Search::new_with_state_limit(20, &policy, 1, variants, 0),
            Err(SearchError::LimitReached(
                PlacementStatus::StateLimitReached
            ))
        ));
    }

    #[test]
    fn deadline_publishes_exact_10_before_later_11() {
        let base = Instant::now();
        let signal = crate::deadline::DeadlineSignal::new();
        let signal_for_cancel = signal.clone();
        let mut calls = 0u64;
        let mut cancel = move || {
            calls += 1;
            if calls == 14 {
                signal_for_cancel.expire();
            }
            false
        };
        let mut clock = DeadlineClock::with_clock_signal(
            base,
            base + Duration::from_secs(5),
            move || base,
            signal,
            &mut cancel,
        );
        let policy = PlacementPolicy::default_for(20);
        let outcome = search(
            20,
            &[range(0, 0, 4), range(0, 0, 5), range(10, 10, 6)],
            2,
            &policy,
            &mut clock,
        )
        .expect("deadline plan");
        assert_eq!(outcome.covered, 10);
        assert_eq!(outcome.best_useful, 10);
        assert_eq!(outcome.status, PlacementStatus::DeadlinePartial);
        assert_eq!(outcome.plan.len(), 2);
        assert_eq!(outcome.plan[0].token_count, 4);
        assert_eq!(outcome.plan[1].token_count, 6);
    }

    #[test]
    fn deadline_publishes_coalesced_10() {
        let base = Instant::now();
        let signal = crate::deadline::DeadlineSignal::new();
        let signal_for_cancel = signal.clone();
        let mut calls = 0u64;
        let mut cancel = move || {
            calls += 1;
            if calls == 9 {
                signal_for_cancel.expire();
            }
            false
        };
        let mut clock = DeadlineClock::with_clock_signal(
            base,
            base + Duration::from_secs(5),
            move || base,
            signal,
            &mut cancel,
        );
        let policy = PlacementPolicy::default_for(20);
        let outcome = search(
            20,
            &[range(0, 0, 4), range(4, 4, 6)],
            2,
            &policy,
            &mut clock,
        )
        .expect("coalesced deadline plan");
        assert_eq!(outcome.covered, 10);
        assert_eq!(outcome.best_useful, 10);
        assert_eq!(outcome.status, PlacementStatus::DeadlinePartial);
        assert_eq!(outcome.plan.len(), 1);
        assert_eq!(outcome.plan[0].token_count, 10);
    }

    #[test]
    fn deadline_during_rejected_trim_rectangle_is_budget_without_search() {
        let base = Instant::now();
        let signal = crate::deadline::DeadlineSignal::new();
        let signal_for_cancel = signal.clone();
        let mut calls = 0u64;
        let mut cancel = move || {
            calls += 1;
            if calls == 4 {
                signal_for_cancel.expire();
            }
            false
        };
        let mut clock = DeadlineClock::with_clock_signal(
            base,
            base + Duration::from_secs(5),
            move || base,
            signal,
            &mut cancel,
        );
        let mut policy = PlacementPolicy::default_for(6001);
        policy.max_trim_left = 6000;
        policy.max_trim_right = 6000;
        policy.minimum_physical_reuse_tokens = 6001;
        let matches = [range(0, 0, 6000)];
        assert!(matches!(
            search(6001, &matches, 2, &policy, &mut clock),
            Err(SearchError::BudgetExhausted)
        ));
    }

    #[test]
    fn cancellation_during_rejected_trim_rectangle_wins() {
        let base = Instant::now();
        let signal = crate::deadline::DeadlineSignal::new();
        let signal_for_cancel = signal.clone();
        let mut calls = 0u64;
        let mut cancel = move || {
            calls += 1;
            if calls == 2 {
                signal_for_cancel.expire();
                true
            } else {
                false
            }
        };
        let mut clock = DeadlineClock::with_clock_signal(
            base,
            base + Duration::from_secs(5),
            move || base,
            signal,
            &mut cancel,
        );
        let mut policy = PlacementPolicy::default_for(6001);
        policy.max_trim_left = 6000;
        policy.max_trim_right = 6000;
        policy.minimum_physical_reuse_tokens = 6001;
        assert!(matches!(
            search(6001, &[range(0, 0, 6000)], 2, &policy, &mut clock),
            Err(SearchError::Cancelled)
        ));
    }

    #[test]
    fn relative_floor_drops_850_and_retains_900_over_980_with_two_pads() {
        let mut policy = PlacementPolicy::default_for(1000);
        policy.max_new_padding = 2;
        policy.pad_token = Some(99);
        policy.allowed_insertions = &[2];
        policy.minimum_prepared_reuse_start = 4;
        policy.max_prepared_tokens = 1002;
        let matches = [range(0, 4, 850), range(0, 4, 900), range(0, 2, 980)];
        let plan = run_at(1000, &matches, 1, &policy);
        assert_eq!(plan.best_useful, 980);
        // 980 loses 10% (>=882) only to plans with less padding; 900 at 0 pads
        // is retained, while 850 is below the floor.
        assert_eq!(plan.covered, 900);
        assert_eq!(plan.padding, 0);
    }

    #[test]
    fn one_hundred_percent_can_choose_empty_after_positive_result() {
        let mut policy = PlacementPolicy::default_for(20);
        policy.loss_allowance = LossAllowance::new(10_000);
        policy.max_new_padding = 1;
        policy.pad_token = Some(99);
        policy.allowed_insertions = &[0];
        policy.prepared_target_start_alignment = 2;
        policy.max_prepared_tokens = 21;
        // The only positive plan needs one pad; at 100% the empty plan wins.
        let matches = [range(0, 1, 5)];
        let outcome = run(&matches, 1, &policy);
        assert_eq!(outcome.covered, 0);
        assert_eq!(outcome.best_useful, 5);
        assert_eq!(outcome.status, PlacementStatus::Complete);
    }
    // Independent small-domain oracle: enumerate subsets, every declared pad
    // count and every allowed insertion boundary, then coalesce. It deliberately
    // does not call production transition/minimal-padding helpers.
    #[derive(Clone)]
    struct OraclePlan {
        covered: u64,
        padding: u32,
        ranges: Vec<PlacedRange>,
    }

    fn oracle_less(a: &OraclePlan, b: &OraclePlan) -> bool {
        if a.covered != b.covered {
            return a.covered > b.covered;
        }
        if a.ranges.len() != b.ranges.len() {
            return a.ranges.len() < b.ranges.len();
        }
        a.ranges
            .iter()
            .map(|r| {
                (
                    r.destination_start,
                    r.source_start,
                    r.token_count,
                    r.prepared_start,
                )
            })
            .lt(b.ranges.iter().map(|r| {
                (
                    r.destination_start,
                    r.source_start,
                    r.token_count,
                    r.prepared_start,
                )
            }))
    }

    #[allow(clippy::too_many_arguments)]
    fn oracle_place(
        context_len: usize,
        policy: &PlacementPolicy<'_>,
        remaining: &[Variant],
        plan: &mut [PlacedRange],
        padding: u32,
        covered: u64,
        k: u32,
        best: &mut Option<OraclePlan>,
    ) {
        if remaining.is_empty() {
            let candidate = OraclePlan {
                covered,
                padding,
                ranges: plan.to_vec(),
            };
            let replace = match best {
                None => true,
                Some(existing) => oracle_less(&candidate, existing),
            };
            if replace {
                *best = Some(candidate);
            }
            return;
        }
        let variant = remaining[0];
        let rest = &remaining[1..];
        let suffix_start = context_len.saturating_sub(policy.fresh_suffix_tokens as usize);
        if variant.destination_start < policy.protected_prefix_tokens
            || variant.destination_start as usize + variant.token_count as usize > suffix_start
            || !variant
                .source_start
                .is_multiple_of(policy.source_start_alignment)
            || !variant
                .token_count
                .is_multiple_of(policy.reuse_count_alignment)
            || variant.token_count < policy.minimum_physical_reuse_tokens
        {
            return;
        }
        let last = plan.last().copied();
        let gap_start = last.map_or(policy.protected_prefix_tokens, |r| r.destination_end());
        if last.is_some_and(|r| {
            r.destination_end() > variant.destination_start || r.source_end() > variant.source_start
        }) {
            return;
        }
        let max_pad = policy.max_new_padding.saturating_sub(padding);
        for extra in 0..=max_pad {
            let prepared = variant.destination_start as u64 + padding as u64 + extra as u64;
            if !prepared.is_multiple_of(policy.prepared_target_start_alignment as u64)
                || prepared < policy.minimum_prepared_reuse_start as u64
                || context_len as u64 + padding as u64 + extra as u64
                    > policy.max_prepared_tokens as u64
            {
                continue;
            }
            let boundaries: Vec<u32> = if extra == 0 {
                vec![0]
            } else {
                policy
                    .allowed_insertions
                    .iter()
                    .copied()
                    .filter(|&b| b >= gap_start && b <= variant.destination_start)
                    .collect()
            };
            for _boundary in boundaries {
                let mut next_plan = plan.to_vec();
                let adjacent = extra == 0
                    && last.is_some_and(|r| {
                        r.destination_end() == variant.destination_start
                            && r.source_end() == variant.source_start
                    });
                if adjacent {
                    next_plan.last_mut().expect("prior range").token_count += variant.token_count;
                } else {
                    if next_plan.len() >= k as usize {
                        continue;
                    }
                    next_plan.push(PlacedRange {
                        source_start: variant.source_start,
                        destination_start: variant.destination_start,
                        token_count: variant.token_count,
                        prepared_start: prepared as u32,
                    });
                }
                if next_plan.len() > k as usize {
                    continue;
                }
                oracle_place(
                    context_len,
                    policy,
                    rest,
                    &mut next_plan,
                    padding + extra,
                    covered + variant.token_count as u64,
                    k,
                    best,
                );
            }
        }
    }

    fn oracle_best(
        context_len: usize,
        variants: &[Variant],
        policy: &PlacementPolicy<'_>,
        k: u32,
    ) -> Option<OraclePlan> {
        if variants.len() > 4 {
            return None;
        }
        let mut best = None;
        for mask in 0u32..(1u32 << variants.len()) {
            if mask == 0 {
                continue;
            }
            let mut selected: Vec<Variant> = (0..variants.len())
                .filter(|&i| mask & (1 << i) != 0)
                .map(|i| variants[i])
                .collect();
            selected.sort_unstable_by_key(|v| (v.destination_start, v.source_start, v.token_count));
            let mut plan = Vec::new();
            oracle_place(
                context_len,
                policy,
                &selected,
                &mut plan,
                0,
                0,
                k,
                &mut best,
            );
        }
        best
    }

    fn independent_select(
        context_len: usize,
        variants: &[Variant],
        policy: &PlacementPolicy<'_>,
        k: u32,
    ) -> Option<OraclePlan> {
        let all = oracle_best(context_len, variants, policy, k)?;
        let best_useful = all.covered;
        let floor = best_useful
            - best_useful.saturating_mul(policy.loss_allowance.basis_points() as u64) / 10_000;
        // Re-run the enumerator but retain the best eligible plan by the real
        // objective: padding, then coverage/ranges/lex.
        let mut selected: Option<OraclePlan> = None;
        #[allow(clippy::too_many_arguments)]
        fn walk(
            context_len: usize,
            policy: &PlacementPolicy<'_>,
            remaining: &[Variant],
            plan: &mut [PlacedRange],
            padding: u32,
            covered: u64,
            floor: u64,
            k: u32,
            selected: &mut Option<OraclePlan>,
        ) {
            if remaining.is_empty() {
                if covered < floor {
                    return;
                }
                let candidate = OraclePlan {
                    covered,
                    padding,
                    ranges: plan.to_vec(),
                };
                let replace = match selected {
                    None => true,
                    Some(existing) => {
                        candidate.padding < existing.padding
                            || (candidate.padding == existing.padding
                                && oracle_less(&candidate, existing))
                    }
                };
                if replace {
                    *selected = Some(candidate);
                }
                return;
            }
            let variant = remaining[0];
            let rest = &remaining[1..];
            let last = plan.last().copied();
            let gap_start = last.map_or(policy.protected_prefix_tokens, |r| r.destination_end());
            if last.is_some_and(|r| {
                r.destination_end() > variant.destination_start
                    || r.source_end() > variant.source_start
            }) {
                return;
            }
            let max_pad = policy.max_new_padding.saturating_sub(padding);
            for extra in 0..=max_pad {
                let prepared = variant.destination_start as u64 + padding as u64 + extra as u64;
                if !prepared.is_multiple_of(policy.prepared_target_start_alignment as u64)
                    || prepared < policy.minimum_prepared_reuse_start as u64
                    || context_len as u64 + padding as u64 + extra as u64
                        > policy.max_prepared_tokens as u64
                {
                    continue;
                }
                let boundaries: Vec<u32> = if extra == 0 {
                    vec![0]
                } else {
                    policy
                        .allowed_insertions
                        .iter()
                        .copied()
                        .filter(|&b| b >= gap_start && b <= variant.destination_start)
                        .collect()
                };
                for _boundary in boundaries {
                    let mut next_plan = plan.to_vec();
                    let adjacent = extra == 0
                        && last.is_some_and(|r| {
                            r.destination_end() == variant.destination_start
                                && r.source_end() == variant.source_start
                        });
                    if adjacent {
                        next_plan.last_mut().expect("prior").token_count += variant.token_count;
                    } else {
                        if next_plan.len() >= k as usize {
                            continue;
                        }
                        next_plan.push(PlacedRange {
                            source_start: variant.source_start,
                            destination_start: variant.destination_start,
                            token_count: variant.token_count,
                            prepared_start: prepared as u32,
                        });
                    }
                    if next_plan.len() > k as usize {
                        continue;
                    }
                    walk(
                        context_len,
                        policy,
                        rest,
                        &mut next_plan,
                        padding + extra,
                        covered + variant.token_count as u64,
                        floor,
                        k,
                        selected,
                    );
                }
            }
        }
        for mask in 1u32..(1u32 << variants.len()) {
            let mut selected_variants: Vec<Variant> = (0..variants.len())
                .filter(|&i| mask & (1 << i) != 0)
                .map(|i| variants[i])
                .collect();
            selected_variants
                .sort_unstable_by_key(|v| (v.destination_start, v.source_start, v.token_count));
            let mut plan = Vec::new();
            walk(
                context_len,
                policy,
                &selected_variants,
                &mut plan,
                0,
                0,
                floor,
                k,
                &mut selected,
            );
        }
        selected
    }

    #[test]
    fn deadline_after_completed_placement_returns_best_partial_plan() {
        let base = Instant::now();
        let signal = crate::deadline::DeadlineSignal::new();
        let signal_for_cancel = signal.clone();
        let mut calls = 0u64;
        let mut cancel = move || {
            calls += 1;
            if calls == 6 {
                signal_for_cancel.expire();
            }
            false
        };
        let mut clock = DeadlineClock::with_clock_signal(
            base,
            base + Duration::from_secs(5),
            move || base,
            signal,
            &mut cancel,
        );
        let policy = PlacementPolicy::default_for(20);
        let matches = [range(0, 0, 4), range(6, 6, 5)];
        match search(20, &matches, 1, &policy, &mut clock) {
            Ok(outcome) => {
                assert_eq!(outcome.covered, 4);
                assert_eq!(outcome.status, PlacementStatus::DeadlinePartial);
            }
            Err(error) => panic!("expected a completed partial plan, got {error:?}"),
        }
    }

    #[test]
    fn hundred_percent_without_a_completed_plan_still_reports_budget() {
        let base = Instant::now();
        let signal = crate::deadline::DeadlineSignal::new();
        signal.expire();
        let mut cancel = || false;
        let mut clock = DeadlineClock::with_clock_signal(
            base,
            base + Duration::from_secs(5),
            move || base,
            signal,
            &mut cancel,
        );
        let mut policy = PlacementPolicy::default_for(20);
        policy.loss_allowance = LossAllowance::new(10_000);
        policy.max_new_padding = 1;
        policy.pad_token = Some(99);
        assert!(matches!(
            search(20, &[range(0, 0, 4)], 1, &policy, &mut clock),
            Err(SearchError::BudgetExhausted)
        ));
    }

    #[test]
    fn early_deadline_before_a_valid_plan_is_budget_not_empty() {
        let base = Instant::now();
        let signal = crate::deadline::DeadlineSignal::new();
        signal.expire();
        let mut cancel = || false;
        let mut clock = DeadlineClock::with_clock_signal(
            base,
            base + Duration::from_secs(5),
            move || base,
            signal,
            &mut cancel,
        );
        let policy = PlacementPolicy::default_for(20);
        assert!(matches!(
            search(20, &[range(0, 0, 4)], 1, &policy, &mut clock),
            Err(SearchError::BudgetExhausted)
        ));
    }

    #[test]
    fn independent_oracle_checks_trim_rectangle_and_padding_counts() {
        let mut policy = PlacementPolicy::default_for(20);
        policy.max_trim_left = 1;
        policy.max_trim_right = 2;
        policy.reuse_count_alignment = 2;
        policy.max_new_padding = 2;
        policy.pad_token = Some(99);
        policy.allowed_insertions = &[
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
        ];
        policy.prepared_target_start_alignment = 2;
        let base_match = CandidateRange {
            source_start: 1,
            destination_start: 2,
            token_count: 7,
        };
        let matches = [base_match];
        let outcome = run_at(20, &matches, 2, &policy);

        // Independently derive the declared trim rectangle.
        let mut variants = Vec::new();
        for left in 0..=policy.max_trim_left {
            for right in 0..=policy.max_trim_right {
                let count = base_match.token_count - left - right;
                if count == 0 {
                    continue;
                }
                let v = Variant {
                    source_start: base_match.source_start + left,
                    destination_start: base_match.destination_start + left,
                    token_count: count,
                };
                if v.source_start.is_multiple_of(policy.source_start_alignment)
                    && v.token_count.is_multiple_of(policy.reuse_count_alignment)
                {
                    variants.push(v);
                }
            }
        }
        let oracle = independent_select(20, &variants, &policy, 2).expect("trim oracle");
        assert_eq!(outcome.covered, oracle.covered);
        assert_eq!(outcome.padding, oracle.padding);
        assert_eq!(outcome.plan.len(), oracle.ranges.len());
    }

    #[test]
    fn oracle_and_search_agree_on_tight_multi_gap_capacity_with_k3() {
        let mut policy = PlacementPolicy::default_for(20);
        policy.max_new_padding = 2;
        policy.pad_token = Some(99);
        policy.allowed_insertions = &[
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
        ];
        policy.prepared_target_start_alignment = 2;
        policy.max_prepared_tokens = 21;
        let variants = vec![
            Variant {
                source_start: 0,
                destination_start: 1,
                token_count: 2,
            },
            Variant {
                source_start: 5,
                destination_start: 4,
                token_count: 2,
            },
            Variant {
                source_start: 10,
                destination_start: 7,
                token_count: 2,
            },
        ];
        let matches: Vec<CandidateRange> = variants
            .iter()
            .map(|v| CandidateRange {
                source_start: v.source_start,
                destination_start: v.destination_start,
                token_count: v.token_count,
            })
            .collect();
        let outcome = run_at(20, &matches, 3, &policy);
        let oracle = independent_select(20, &variants, &policy, 3).expect("tight oracle");
        assert_eq!(outcome.covered, oracle.covered);
        assert_eq!(outcome.padding, oracle.padding);
        assert_eq!(outcome.plan.len(), oracle.ranges.len());
        assert!(outcome.padding <= 1);
    }

    #[test]
    fn independent_oracle_agrees_on_small_no_pad_and_padded_domains() {
        let policy = PlacementPolicy::default_for(30);
        let variants = vec![
            Variant {
                source_start: 0,
                destination_start: 0,
                token_count: 4,
            },
            Variant {
                source_start: 4,
                destination_start: 4,
                token_count: 4,
            },
            Variant {
                source_start: 0,
                destination_start: 2,
                token_count: 3,
            },
            Variant {
                source_start: 10,
                destination_start: 10,
                token_count: 5,
            },
        ];
        let matches: Vec<CandidateRange> = variants
            .iter()
            .map(|v| CandidateRange {
                source_start: v.source_start,
                destination_start: v.destination_start,
                token_count: v.token_count,
            })
            .collect();
        let outcome = run_at(30, &matches, 2, &policy);
        let oracle = independent_select(30, &variants, &policy, 2).expect("oracle plan");
        assert_eq!(outcome.covered, oracle.covered);
        assert_eq!(outcome.padding, oracle.padding);
        assert_eq!(outcome.plan.len(), oracle.ranges.len());

        let mut policy = PlacementPolicy::default_for(20);
        policy.max_new_padding = 2;
        policy.pad_token = Some(99);
        policy.allowed_insertions = &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        policy.prepared_target_start_alignment = 2;
        let variants = vec![
            Variant {
                source_start: 0,
                destination_start: 1,
                token_count: 4,
            },
            Variant {
                source_start: 4,
                destination_start: 5,
                token_count: 4,
            },
            Variant {
                source_start: 0,
                destination_start: 4,
                token_count: 3,
            },
        ];
        let matches: Vec<CandidateRange> = variants
            .iter()
            .map(|v| CandidateRange {
                source_start: v.source_start,
                destination_start: v.destination_start,
                token_count: v.token_count,
            })
            .collect();
        let outcome = run_at(20, &matches, 2, &policy);
        let oracle = independent_select(20, &variants, &policy, 2).expect("padded oracle plan");
        assert_eq!(outcome.covered, oracle.covered);
        assert_eq!(outcome.padding, oracle.padding);
        assert_eq!(outcome.plan.len(), oracle.ranges.len());
    }
}
