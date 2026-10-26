//! Bounded placement-aware search over actual-token maximal matches.
//!
//! Discovery supplies maximal exact matches. This module derives the declared
//! trim variants, applies the canonical insertion rule for each fresh gap, and
//! searches over continuation states. It is the only search pipeline used by
//! [`crate::CacheReuseAnalysis`].
//!
//! Search pipeline:
//! 1. derive bounded trim variants from maximal matches;
//! 2. place those variants and extend valid plans without crossing ranges;
//! 3. publish completed best-by-padding plans into buckets, while retaining
//!    continuation frontiers separately;
//! 4. compute the best useful coverage `B` and its relative admissible floor;
//! 5. choose the least-padding eligible bucket for the returned plan.

use std::collections::{HashMap, HashSet};
use std::mem::size_of;

use crate::candidate::CandidateRange;
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
/// against this 4 MiB reserve. Allocator overhead, the `HashMap` table and other
/// spare capacity are excluded. This is modeled planner payload, not measured
/// whole-process memory; in particular it is not RSS (resident set size, the
/// operating system's measure of resident process memory) and says nothing
/// about GPU allocation.
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

/// Identity of one retained continuation frontier: its last placed range,
/// final range count and current padding. Buckets below hold completed plans;
/// `StateKey` entries hold plans that may still be extended.
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
    /// Continuation frontiers that may still be extended by later variants.
    states: HashMap<StateKey, Partial>,
    order: Vec<StateKey>,
    /// Best completed plan seen for each padding count. These are final
    /// candidates, distinct from the extension states above.
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

        // Coalescing is part of the K contract: adjacent compatible ranges
        // become one final range, so a plan can stay within K without changing
        // its covered tokens. Non-adjacent ranges consume a K slot below.
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
    /// The bucket is a completed plan and is updated first; only then is the
    /// plan admitted as a continuation state. If modeled extension storage
    /// reaches its limit, the completed bucket remains available. Actual
    /// allocator admission errors propagate unchanged.
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

#[cfg(test)]
mod tests;
