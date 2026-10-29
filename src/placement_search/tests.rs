use super::*;
use crate::candidate::{CandidateRange, MAX_RANGES};
use crate::placement::LossAllowance;
use crate::timing::Stopper;
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

fn range(source_start: u32, destination_start: u32, token_count: u32) -> CandidateRange {
    CandidateRange {
        source_start,
        destination_start,
        token_count,
    }
}

fn clock<'a>(deadline: Instant, cancel: &'a mut dyn FnMut() -> bool) -> Stopper<'a> {
    let base = Instant::now();
    Stopper::with_clock(deadline, cancel, move |_| base)
}

/// Manual deadline: the clock reads as expired once `expire` is called.
#[derive(Clone, Default)]
struct Expiry(Rc<Cell<bool>>);

impl Expiry {
    fn expire(&self) {
        self.0.set(true);
    }
}

fn expiring_clock<'a>(
    base: Instant,
    expiry: Expiry,
    cancel: &'a mut dyn FnMut() -> bool,
) -> Stopper<'a> {
    let deadline = base + Duration::from_secs(5);
    Stopper::with_clock(deadline, cancel, move |_| {
        if expiry.0.get() { deadline } else { base }
    })
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
    PlacementPolicy::without_padding(context_len)
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
    let policy = PlacementPolicy::without_padding(20);
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
    let policy = PlacementPolicy::without_padding(20);
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
    let policy = PlacementPolicy::without_padding(20);
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
    let signal = Expiry::default();
    let signal_for_cancel = signal.clone();
    let mut calls = 0u64;
    let mut cancel = move || {
        calls += 1;
        if calls == 14 {
            signal_for_cancel.expire();
        }
        false
    };
    let mut clock = expiring_clock(base, signal, &mut cancel);
    let policy = PlacementPolicy::without_padding(20);
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
    let signal = Expiry::default();
    let signal_for_cancel = signal.clone();
    let mut calls = 0u64;
    let mut cancel = move || {
        calls += 1;
        if calls == 9 {
            signal_for_cancel.expire();
        }
        false
    };
    let mut clock = expiring_clock(base, signal, &mut cancel);
    let policy = PlacementPolicy::without_padding(20);
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
    let signal = Expiry::default();
    let signal_for_cancel = signal.clone();
    let mut calls = 0u64;
    let mut cancel = move || {
        calls += 1;
        if calls == 4 {
            signal_for_cancel.expire();
        }
        false
    };
    let mut clock = expiring_clock(base, signal, &mut cancel);
    let mut policy = PlacementPolicy::without_padding(6001);
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
    let signal = Expiry::default();
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
    let mut clock = expiring_clock(base, signal, &mut cancel);
    let mut policy = PlacementPolicy::without_padding(6001);
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
    let mut policy = PlacementPolicy::without_padding(1000);
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
    let mut policy = PlacementPolicy::without_padding(20);
    policy.loss_allowance = LossAllowance::from_basis_points(10_000);
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
    let signal = Expiry::default();
    let signal_for_cancel = signal.clone();
    let mut calls = 0u64;
    let mut cancel = move || {
        calls += 1;
        if calls == 6 {
            signal_for_cancel.expire();
        }
        false
    };
    let mut clock = expiring_clock(base, signal, &mut cancel);
    let policy = PlacementPolicy::without_padding(20);
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
    let signal = Expiry::default();
    signal.expire();
    let mut cancel = || false;
    let mut clock = expiring_clock(base, signal, &mut cancel);
    let mut policy = PlacementPolicy::without_padding(20);
    policy.loss_allowance = LossAllowance::from_basis_points(10_000);
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
    let signal = Expiry::default();
    signal.expire();
    let mut cancel = || false;
    let mut clock = expiring_clock(base, signal, &mut cancel);
    let policy = PlacementPolicy::without_padding(20);
    assert!(matches!(
        search(20, &[range(0, 0, 4)], 1, &policy, &mut clock),
        Err(SearchError::BudgetExhausted)
    ));
}

#[test]
fn independent_oracle_checks_trim_rectangle_and_padding_counts() {
    let mut policy = PlacementPolicy::without_padding(20);
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
    let mut policy = PlacementPolicy::without_padding(20);
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
    let policy = PlacementPolicy::without_padding(30);
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

    let mut policy = PlacementPolicy::without_padding(20);
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
