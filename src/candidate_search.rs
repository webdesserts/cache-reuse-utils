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
