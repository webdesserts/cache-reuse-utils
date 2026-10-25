//! Internal candidate search: DP, coalescing and the private search wrapper.

use std::mem::size_of;

use crate::candidate::{CandidateRange, MAX_CANDIDATES, MAX_RANGES};
use crate::timing::{CheckOutcome, DeadlineClock};

const NONE: u32 = u32::MAX;

#[derive(Debug, Clone, Copy)]
struct SearchState {
    covered: u64,
    previous: u32,
}

fn normalized_less(a: &CandidateRange, b: &CandidateRange) -> bool {
    (a.destination_start, a.source_start, a.token_count)
        < (b.destination_start, b.source_start, b.token_count)
}

fn compatible(previous: &CandidateRange, next: &CandidateRange) -> bool {
    previous
        .source_end()
        .is_some_and(|end| end <= next.source_start)
        && previous
            .destination_end()
            .is_some_and(|end| end <= next.destination_start)
}

fn adjacent(previous: &CandidateRange, next: &CandidateRange) -> bool {
    previous.source_end() == Some(next.source_start)
        && previous.destination_end() == Some(next.destination_start)
}

/// Modeled peak payload for the DP states, order/path tables and the three
/// bounded plan buffers. This excludes spare `Vec` capacity (capacity can round
/// up, especially for small N/K), allocator overhead and stack; native admission
/// uses this model, not measured heap/RSS. The 1 MiB reference ceiling covers
/// this modeled payload, not a platform-wide allocation proof.
pub fn scratch_bytes(count: usize, k: u32) -> Option<u64> {
    if k == 0 || k > MAX_RANGES || count > MAX_CANDIDATES {
        return None;
    }
    if count == 0 {
        return Some(0);
    }
    let states =
        (count as u64).checked_mul((k as u64 + 1).checked_mul(size_of::<SearchState>() as u64)?)?;
    // One sorted order table and one reconstruction path, both usize per candidate.
    // Three bounded plan buffers coexist during a tie check: best incumbent,
    // candidate plan and existing plan.
    let tables = (count as u64).checked_mul((size_of::<usize>() * 2) as u64)?;
    let plan = 3u64
        .checked_mul(k as u64)?
        .checked_mul(size_of::<CandidateRange>() as u64)?;
    states.checked_add(tables)?.checked_add(plan)
}

fn candidate_plan_less(a: &[CandidateRange], b: &[CandidateRange]) -> bool {
    for (left, right) in a.iter().zip(b) {
        if left != right {
            return normalized_less(left, right);
        }
    }
    a.len() < b.len()
}

#[derive(Default)]
struct Search<'a> {
    candidates: &'a [CandidateRange],
    order: Vec<usize>,
    states: Vec<SearchState>,
    k: u32,
    best_state: u32,
    best_b: u32,
    best_plan: Vec<CandidateRange>,
}

fn push_plan(ranges: &mut Vec<CandidateRange>, range: CandidateRange) -> Result<(), SearchError> {
    if let Some(last) = ranges.last_mut()
        && adjacent(last, &range)
    {
        last.token_count =
            last.token_count
                .checked_add(range.token_count)
                .ok_or(SearchError::Invariant(
                    "coalesced token count overflows u32",
                ))?;
        return Ok(());
    }
    ranges.push(range);
    Ok(())
}

impl<'a> Search<'a> {
    fn state_index(&self, candidate: usize, b: u32) -> u32 {
        (candidate as u32) * (self.k + 1) + b
    }

    fn materialize(&self, state: u32) -> Result<Vec<CandidateRange>, SearchError> {
        let mut indices = Vec::new();
        indices
            .try_reserve(self.candidates.len().min(self.order.len()))
            .map_err(|_| SearchError::AdmissionDenied)?;
        let mut key = state;
        while key != NONE {
            let candidate_index = (key / (self.k + 1)) as usize;
            indices.push(self.order[candidate_index]);
            let previous = self.states[key as usize].previous;
            if previous != NONE && previous >= key {
                return Err(SearchError::Invariant("predecessor did not move forward"));
            }
            key = previous;
        }
        let mut ranges = Vec::new();
        ranges
            .try_reserve(self.k as usize)
            .map_err(|_| SearchError::AdmissionDenied)?;
        for candidate in indices.into_iter().rev() {
            push_plan(&mut ranges, self.candidates[candidate])?;
        }
        if ranges.len() > self.k as usize {
            return Err(SearchError::Invariant("coalesced plan exceeds K"));
        }
        Ok(ranges)
    }

    fn try_record(&mut self, state: u32, b: u32) -> Result<(), SearchError> {
        let covered = self.states[state as usize].covered;
        if covered == 0 {
            return Ok(());
        }
        let plan = self.materialize(state)?;
        if covered != plan.iter().map(|r| r.token_count as u64).sum::<u64>() {
            return Err(SearchError::Invariant(
                "state coverage disagrees with its plan",
            ));
        }
        let better = covered
            > self
                .best_plan
                .iter()
                .map(|r| r.token_count as u64)
                .sum::<u64>()
            || (covered
                == self
                    .best_plan
                    .iter()
                    .map(|r| r.token_count as u64)
                    .sum::<u64>()
                && (b < self.best_b
                    || (b == self.best_b && candidate_plan_less(&plan, &self.best_plan))));
        if better {
            self.best_state = state;
            self.best_b = b;
            self.best_plan = plan;
        }
        Ok(())
    }
}

/// Internal search failure. The caller-facing analysis wrapper adds stage and
/// timing metadata when it turns this into a [`PlanError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchError {
    Cancelled,
    BudgetExhausted,
    AdmissionDenied,
    Invariant(&'static str),
}

/// Run the bounded candidate search against the supplied absolute deadline
/// clock. The clock is shared with preparation so one monotonic deadline covers
/// the whole analysis. Cancellation is polled first and always wins; on deadline
/// the best already-found valid ranges are returned, with no further candidate
/// exploration. The caller builds the final [`ReusePlan`].
pub(crate) fn search(
    candidates: &[CandidateRange],
    k: u32,
    clock: &mut DeadlineClock<'_>,
) -> Result<Vec<CandidateRange>, SearchError> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let mut search = Search {
        candidates,
        k,
        order: Vec::new(),
        states: Vec::new(),
        best_state: NONE,
        best_b: 0,
        best_plan: Vec::new(),
    };
    let state_len = candidates
        .len()
        .checked_mul(k as usize + 1)
        .ok_or(SearchError::Invariant("state table length overflows"))?;
    search
        .order
        .try_reserve_exact(candidates.len())
        .map_err(|_| SearchError::AdmissionDenied)?;
    search
        .states
        .try_reserve_exact(state_len)
        .map_err(|_| SearchError::AdmissionDenied)?;
    search.order.extend(0..candidates.len());
    search.order.sort_unstable_by(|&left, &right| {
        let a = &candidates[left];
        let b = &candidates[right];
        (a.destination_start, a.source_start, a.token_count, left).cmp(&(
            b.destination_start,
            b.source_start,
            b.token_count,
            right,
        ))
    });
    search.states.resize(
        state_len,
        SearchState {
            covered: 0,
            previous: NONE,
        },
    );

    let mut stopped_for_deadline = false;
    'outer: for j in 0..candidates.len() {
        match clock.check() {
            CheckOutcome::Cancelled => return Err(SearchError::Cancelled),
            CheckOutcome::DeadlineReached => {
                stopped_for_deadline = true;
                break 'outer;
            }
            CheckOutcome::Continue => {}
        }
        let current = candidates[search.order[j]];
        let first = search.state_index(j, 1);
        search.states[first as usize] = SearchState {
            covered: current.token_count as u64,
            previous: NONE,
        };
        search.try_record(first, 1)?;

        for i in 0..j {
            match clock.check() {
                CheckOutcome::Cancelled => return Err(SearchError::Cancelled),
                CheckOutcome::DeadlineReached => {
                    stopped_for_deadline = true;
                    // Publish the already-computed states of this row before
                    // leaving, because end-of-row publication is skipped on this
                    // exit. At most K materializations, no new candidate
                    // exploration.
                    for b in 1..=k {
                        let state = search.state_index(j, b);
                        if search.states[state as usize].covered != 0 {
                            search.try_record(state, b)?;
                        }
                    }
                    break 'outer;
                }
                CheckOutcome::Continue => {}
            }
            let previous = candidates[search.order[i]];
            if !compatible(&previous, &current) {
                continue;
            }
            let extra = if adjacent(&previous, &current) { 0 } else { 1 };
            for b in 1..=k - extra {
                let from = search.state_index(i, b);
                if search.states[from as usize].covered == 0 {
                    continue;
                }
                let to = search.state_index(j, b + extra);
                let score = search.states[from as usize].covered + current.token_count as u64;
                let existing = search.states[to as usize];
                if score > existing.covered {
                    search.states[to as usize] = SearchState {
                        covered: score,
                        previous: from,
                    };
                } else if score == existing.covered && existing.covered != 0 {
                    let candidate_plan = {
                        let mut ranges = search.materialize(from)?;
                        push_plan(&mut ranges, current)?;
                        ranges
                    };
                    let existing_plan = search.materialize(to)?;
                    if candidate_plan_less(&candidate_plan, &existing_plan) {
                        search.states[to as usize] = SearchState {
                            covered: score,
                            previous: from,
                        };
                    }
                }
            }
        }
        for b in 1..=k {
            let state = search.state_index(j, b);
            if search.states[state as usize].covered != 0 {
                search.try_record(state, b)?;
            }
        }
    }

    match clock.check() {
        CheckOutcome::Cancelled => return Err(SearchError::Cancelled),
        CheckOutcome::DeadlineReached => stopped_for_deadline = true,
        CheckOutcome::Continue => {}
    }
    if search.best_state == NONE {
        return if stopped_for_deadline {
            Err(SearchError::BudgetExhausted)
        } else {
            Err(SearchError::Invariant(
                "search finished without a plan or a deadline stop",
            ))
        };
    }
    if search.best_plan.len() > k as usize {
        return Err(SearchError::Invariant("selected plan exceeds K"));
    }
    Ok(search.best_plan)
}

/// Private translation of the search algorithm for the public analysis
/// operation. It owns no public surface and publishes no alternative search API.
pub(crate) struct CandidateSearch<'a> {
    candidates: &'a [CandidateRange],
    k: u32,
}

impl<'a> CandidateSearch<'a> {
    pub(crate) fn new(candidates: &'a [CandidateRange], k: u32) -> Self {
        Self { candidates, k }
    }

    pub(crate) fn run(
        &self,
        clock: &mut DeadlineClock<'_>,
    ) -> Result<Vec<CandidateRange>, SearchError> {
        search(self.candidates, self.k, clock)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate::CandidateRange;
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

    fn fixed_clock(base: Instant) -> impl FnMut() -> Instant {
        move || base
    }

    fn search_plan(
        candidates: &[CandidateRange],
        k: u32,
        clock: &mut DeadlineClock<'_>,
    ) -> Vec<CandidateRange> {
        search(candidates, k, clock).expect("search plan")
    }

    fn select(candidates: &[CandidateRange], k: u32) -> Vec<CandidateRange> {
        let base = Instant::now();
        let mut cancel = || false;
        let mut clock = DeadlineClock::with_clock(
            base,
            base + Duration::from_secs(5),
            fixed_clock(base),
            &mut cancel,
        );
        search_plan(candidates, k, &mut clock)
    }

    #[test]
    fn deadline_publishes_computed_improvement_in_the_current_row() {
        let base = Instant::now();
        let signal = crate::deadline::DeadlineSignal::new();
        let cancel_signal = signal.clone();
        let calls = Rc::new(Cell::new(0u64));
        let cancel_calls = Rc::clone(&calls);
        let mut cancel = move || {
            cancel_calls.set(cancel_calls.get() + 1);
            if cancel_calls.get() == 6 {
                cancel_signal.expire();
            }
            false
        };
        let mut clock = DeadlineClock::with_clock_signal(
            base,
            base + Duration::from_secs(5),
            fixed_clock(base),
            signal,
            &mut cancel,
        );
        let candidates = [range(0, 0, 4), range(0, 0, 5), range(10, 10, 6)];
        let plan = search_plan(&candidates, 2, &mut clock);
        assert_eq!(plan.iter().map(|r| r.token_count as u64).sum::<u64>(), 10);
    }

    #[test]
    fn deadline_publishes_coalesced_improvement_in_the_current_row() {
        let base = Instant::now();
        let signal = crate::deadline::DeadlineSignal::new();
        let cancel_signal = signal.clone();
        let calls = Rc::new(Cell::new(0u64));
        let cancel_calls = Rc::clone(&calls);
        let mut cancel = move || {
            cancel_calls.set(cancel_calls.get() + 1);
            if cancel_calls.get() == 6 {
                cancel_signal.expire();
            }
            false
        };
        let mut clock = DeadlineClock::with_clock_signal(
            base,
            base + Duration::from_secs(5),
            fixed_clock(base),
            signal,
            &mut cancel,
        );
        let candidates = [range(0, 0, 4), range(0, 0, 5), range(4, 4, 6)];
        let plan = search_plan(&candidates, 2, &mut clock);
        assert_eq!(plan.iter().map(|r| r.token_count as u64).sum::<u64>(), 10);
        assert_eq!(plan, vec![range(0, 0, 10)]);
    }

    #[test]
    fn full_search_controls_return_the_best_available_plans() {
        let base = Instant::now();
        let mut cancel = || false;
        let mut clock = DeadlineClock::with_clock(
            base,
            base + Duration::from_secs(1),
            fixed_clock(base),
            &mut cancel,
        );
        let first = [range(0, 0, 4), range(0, 0, 5), range(10, 10, 6)];
        let plan = search_plan(&first, 2, &mut clock);
        assert_eq!(plan.iter().map(|r| r.token_count as u64).sum::<u64>(), 11);
        drop(clock);

        let mut clock = DeadlineClock::with_clock(
            base,
            base + Duration::from_secs(1),
            fixed_clock(base),
            &mut cancel,
        );
        let second = [range(0, 0, 4), range(0, 0, 5), range(4, 4, 6)];
        let plan = search_plan(&second, 2, &mut clock);
        assert_eq!(plan.iter().map(|r| r.token_count as u64).sum::<u64>(), 10);
    }

    #[test]
    fn expiry_in_search_without_an_incumbent_is_budget_exhausted() {
        let base = Instant::now();
        let signal = crate::deadline::DeadlineSignal::new();
        let cancel_signal = signal.clone();
        let mut cancel = move || {
            cancel_signal.expire();
            false
        };
        let mut clock = DeadlineClock::with_clock_signal(
            base,
            base + Duration::from_secs(5),
            fixed_clock(base),
            signal,
            &mut cancel,
        );
        assert!(matches!(
            search(&[range(0, 0, 4)], 1, &mut clock),
            Err(SearchError::BudgetExhausted)
        ));
    }

    #[test]
    fn cancellation_at_the_deadline_is_cancelled() {
        let base = Instant::now();
        let signal = crate::deadline::DeadlineSignal::new();
        let cancel_signal = signal.clone();
        let calls = Rc::new(Cell::new(0u64));
        let cancel_calls = Rc::clone(&calls);
        let mut cancel = move || {
            cancel_calls.set(cancel_calls.get() + 1);
            if cancel_calls.get() >= 6 {
                cancel_signal.expire();
                true
            } else {
                false
            }
        };
        let mut clock = DeadlineClock::with_clock_signal(
            base,
            base + Duration::from_secs(5),
            fixed_clock(base),
            signal,
            &mut cancel,
        );
        assert!(matches!(
            search(
                &[range(0, 0, 4), range(0, 0, 5), range(10, 10, 6)],
                2,
                &mut clock,
            ),
            Err(SearchError::Cancelled)
        ));
    }

    // Independent exhaustive oracle: enumerate every subset, coalesce, and
    // require the final coalesced count to fit K before scoring.
    fn key(range: &CandidateRange) -> (u32, u32, u32) {
        (
            range.destination_start,
            range.source_start,
            range.token_count,
        )
    }

    fn coalesce(mut selected: Vec<CandidateRange>) -> Vec<CandidateRange> {
        selected.sort_by_key(key);
        let mut plan: Vec<CandidateRange> = Vec::new();
        for candidate in selected {
            if let Some(last) = plan.last_mut()
                && last.source_end() == Some(candidate.source_start)
                && last.destination_end() == Some(candidate.destination_start)
            {
                last.token_count += candidate.token_count;
            } else {
                plan.push(candidate);
            }
        }
        plan
    }

    fn plan_less(a: &[CandidateRange], b: &[CandidateRange]) -> bool {
        for (left, right) in a.iter().zip(b) {
            if left != right {
                return key(left) < key(right);
            }
        }
        a.len() < b.len()
    }

    fn oracle(candidates: &[CandidateRange], k: u32) -> Option<Vec<CandidateRange>> {
        let mut best: Option<Vec<CandidateRange>> = None;
        let mut best_score = 0u64;
        for mask in 0u32..(1u32 << candidates.len()) {
            let selected: Vec<_> = (0..candidates.len())
                .filter(|index| mask & (1 << index) != 0)
                .map(|index| candidates[index])
                .collect();
            let plan = coalesce(selected);
            if plan.len() > k as usize || crate::validate_plan(&plan, k).is_err() {
                continue;
            }
            let score: u64 = plan.iter().map(|range| range.token_count as u64).sum();
            let better = score > best_score
                || (score == best_score
                    && match &best {
                        None => true,
                        Some(current) => {
                            plan.len() < current.len()
                                || (plan.len() == current.len() && plan_less(&plan, current))
                        }
                    });
            if better {
                best = Some(plan);
                best_score = score;
            }
        }
        best
    }

    #[test]
    fn competing_candidates_choose_best_coverage() {
        let candidates = [range(0, 0, 8), range(20, 0, 10), range(40, 20, 3)];
        let one = select(&candidates, 1);
        assert_eq!(one, vec![range(20, 0, 10)]);
        let two = select(&candidates, 2);
        assert_eq!(two.iter().map(|r| r.token_count as u64).sum::<u64>(), 13);
        assert_eq!(two, vec![range(20, 0, 10), range(40, 20, 3)]);
    }

    #[test]
    fn adjacent_selected_ranges_are_coalesced() {
        let one = select(&[range(0, 0, 4), range(4, 4, 3)], 1);
        assert_eq!(one, vec![range(0, 0, 7)]);
        let nodes = [
            range(0, 0, 2),
            range(2, 2, 2),
            range(10, 10, 2),
            range(12, 12, 2),
            range(20, 20, 1),
        ];
        let two = select(&nodes, 2);
        assert_eq!(two.len(), 2);
        assert_eq!(two, vec![range(0, 0, 4), range(10, 10, 4)]);
    }

    #[test]
    fn result_is_independent_of_candidate_order() {
        let candidates = [range(0, 0, 8), range(20, 0, 10), range(40, 20, 3)];
        let expected = select(&candidates, 2);
        let mut reversed = candidates;
        reversed.reverse();
        assert_eq!(select(&reversed, 2), expected);
    }

    #[test]
    fn matches_exhaustive_oracle_on_fixed_and_generated_pools() {
        let fixed = [
            range(0, 0, 4),
            range(4, 4, 3),
            range(10, 10, 2),
            range(12, 12, 2),
            range(20, 20, 1),
            range(30, 10, 5),
        ];
        for k in 1..=3 {
            assert_eq!(select(&fixed, k), oracle(&fixed, k).unwrap(), "k={k}");
        }
        let mut state = 0x1234_5678u64;
        let mut next = || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((state >> 33) % 31) as u32
        };
        for case in 0..30 {
            let len = 3 + (case % 6) as usize;
            let candidates: Vec<_> = (0..len)
                .map(|_| range(next(), next(), 1 + next() % 4))
                .collect();
            let k = 1 + (case % 3) as u32;
            assert_eq!(
                select(&candidates, k),
                oracle(&candidates, k).unwrap(),
                "case {case}"
            );
        }
    }
}
