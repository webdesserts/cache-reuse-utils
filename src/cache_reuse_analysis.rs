//! Public one-shot cache-reuse analysis and its checkpoint orchestration.

use std::time::Instant;

use crate::candidate::{CandidateRange, MAX_SCRATCH_BYTES, STACK_RESERVE_BYTES, validate_pool};
use crate::candidate_search::{CandidateSearch, SearchError};
use crate::error::{PlanError, geometry_reason};
use crate::reuse_plan::ReusePlan;
use crate::timing::{AnalysisStage, AnalysisTiming, CheckOutcome, DeadlineClock};

/// One cache-reuse analysis request.
///
/// This is the README entry point. Today callers supply candidate ranges
/// directly; a later version will take incoming context tokens and a read-only
/// cache view, run discovery, then search. Discovery remains unimplemented, so
/// no cache snapshot or matching is fabricated here.
///
/// `deadline` is one absolute monotonic instant shared by preparation and
/// search; it is not a per-stage budget. `cancel` is a whole-request stop that
/// takes precedence over the deadline. Call [`analyze`](Self::analyze).
pub struct CacheReuseAnalysis<'a> {
    pub candidates: &'a [CandidateRange],
    pub k: u32,
    pub deadline: Instant,
    pub cancel: &'a mut dyn FnMut() -> bool,
}

impl<'a> CacheReuseAnalysis<'a> {
    /// Validate input and search for the best valid plan before `deadline`.
    ///
    /// A valid plan already found is returned as `Ok` even when the deadline
    /// stopped further search. With no plan found before the deadline this is
    /// [`PlanError::BudgetExhausted`]. Deliberately empty input is valid and
    /// returns an empty plan; cancellation wins and never returns a plan.
    pub fn analyze(self) -> Result<ReusePlan, PlanError> {
        let CacheReuseAnalysis {
            candidates,
            k,
            deadline,
            cancel,
        } = self;
        let mut clock = DeadlineClock::start(deadline, cancel);
        analyze_with_clock(candidates, k, &mut clock)
    }
}

fn analyze_with_clock(
    candidates: &[CandidateRange],
    k: u32,
    clock: &mut DeadlineClock<'_>,
) -> Result<ReusePlan, PlanError> {
    // Cancellation is checked before validation and before any deadline work.
    if clock.cancelled() {
        return Err(error_at(
            clock,
            AnalysisStage::Preparation,
            None,
            SearchError::Cancelled,
        ));
    }
    let validated = match validate_pool(candidates, k) {
        Ok(validated) => validated,
        Err(reason) => {
            let finish = clock.now();
            if clock.cancelled() {
                return Err(PlanError::Cancelled {
                    stage: AnalysisStage::Preparation,
                    timing: timing(clock, None, finish),
                });
            }
            return Err(PlanError::InvalidInput {
                reason,
                stage: AnalysisStage::Preparation,
                timing: timing(clock, None, finish),
            });
        }
    };
    if validated.reserved_bytes() > MAX_SCRATCH_BYTES + STACK_RESERVE_BYTES {
        return Err(error_at(
            clock,
            AnalysisStage::Preparation,
            None,
            SearchError::AdmissionDenied,
        ));
    }

    // Empty input is the explicit no-reuse request: there is nothing to search,
    // so an expired deadline does not invent a failure. Cancellation still wins.
    if candidates.is_empty() {
        let finish = clock.now();
        if clock.cancelled() {
            return Err(PlanError::Cancelled {
                stage: AnalysisStage::Preparation,
                timing: timing(clock, None, finish),
            });
        }
        let timing = timing(clock, None, finish);
        return ReusePlan::from_parts(Vec::new(), k, timing).map_err(|error| {
            PlanError::Invariant {
                reason: geometry_reason(error),
                stage: AnalysisStage::Finish,
                timing,
            }
        });
    }

    // Preparation checkpoint: validation and admission are inside deadline
    // awareness, and a past deadline stops before search starts.
    match clock.check() {
        CheckOutcome::Cancelled => {
            return Err(error_at(
                clock,
                AnalysisStage::Preparation,
                None,
                SearchError::Cancelled,
            ));
        }
        CheckOutcome::DeadlineReached => {
            return Err(error_at(
                clock,
                AnalysisStage::Preparation,
                None,
                SearchError::BudgetExhausted,
            ));
        }
        CheckOutcome::Continue => {}
    }

    let search_start = clock.now();
    let search = CandidateSearch::new(candidates, k);
    match search.run(clock) {
        Err(error) => {
            let finish = clock.now();
            Err(error_at_with_finish(
                clock,
                AnalysisStage::Search,
                Some(search_start),
                finish,
                error,
            ))
        }
        Ok(ranges) => {
            let finish = clock.now();
            let timing = timing(clock, Some(search_start), finish);
            // Search already polls cancellation last; this catches a caller
            // that flips it between that poll and plan construction.
            if clock.cancelled() {
                return Err(PlanError::Cancelled {
                    stage: AnalysisStage::Finish,
                    timing,
                });
            }
            ReusePlan::from_parts(ranges, k, timing).map_err(|error| PlanError::Invariant {
                reason: geometry_reason(error),
                stage: AnalysisStage::Finish,
                timing,
            })
        }
    }
}

fn timing(
    clock: &DeadlineClock<'_>,
    search_start: Option<Instant>,
    finish: Instant,
) -> AnalysisTiming {
    let started = clock.started();
    let elapsed = finish.saturating_duration_since(started);
    let preparation = search_start
        .map(|start| start.saturating_duration_since(started))
        .unwrap_or(elapsed);
    let search = search_start.map(|start| finish.saturating_duration_since(start));
    AnalysisTiming::new(
        elapsed,
        clock.limit(),
        finish >= clock.deadline(),
        preparation,
        search,
    )
}

fn error_at(
    clock: &mut DeadlineClock<'_>,
    stage: AnalysisStage,
    search_start: Option<Instant>,
    error: SearchError,
) -> PlanError {
    let finish = clock.now();
    error_at_with_finish(clock, stage, search_start, finish, error)
}

fn error_at_with_finish(
    clock: &DeadlineClock<'_>,
    stage: AnalysisStage,
    search_start: Option<Instant>,
    finish: Instant,
    error: SearchError,
) -> PlanError {
    let timing = timing(clock, search_start, finish);
    match error {
        SearchError::Cancelled => PlanError::Cancelled { stage, timing },
        SearchError::BudgetExhausted => PlanError::BudgetExhausted { stage, timing },
        SearchError::AdmissionDenied => PlanError::AdmissionDenied { stage, timing },
        SearchError::Invariant(reason) => PlanError::Invariant {
            reason,
            stage,
            timing,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    fn range(source_start: u32, destination_start: u32, token_count: u32) -> CandidateRange {
        CandidateRange {
            source_start,
            destination_start,
            token_count,
        }
    }

    /// Clock that advances 1 ms per read and caps at `limit_ms`. The trace is
    /// deterministic, including checkpoint reads; tests below state the exact
    /// stopping point rather than accepting any valid result. The stop point is
    /// intentionally coupled to checkpoint read order; this is not a semantic
    /// clock abstraction.
    fn advancing_clock(base: Instant, limit_ms: u64) -> (Rc<Cell<u64>>, impl FnMut() -> Instant) {
        let calls = Rc::new(Cell::new(0u64));
        let counter = Rc::clone(&calls);
        let now = move || {
            let elapsed = counter.get().min(limit_ms);
            counter.set(counter.get() + 1);
            base + std::time::Duration::from_millis(elapsed)
        };
        (calls, now)
    }

    fn fixed_clock(base: Instant) -> impl FnMut() -> Instant {
        move || base
    }

    #[test]
    fn expiry_during_preparation_stops_before_search() {
        let base = Instant::now();
        let mut cancel = || false;
        let mut clock = DeadlineClock::with_clock(
            base,
            base + std::time::Duration::from_millis(5),
            move || base + std::time::Duration::from_millis(5),
            &mut cancel,
        );
        match analyze_with_clock(&[range(0, 0, 4)], 1, &mut clock) {
            Err(PlanError::BudgetExhausted { stage, timing }) => {
                assert_eq!(stage, AnalysisStage::Preparation);
                assert!(timing.reached());
                assert!(timing.search().is_none());
                assert_eq!(timing.preparation(), timing.elapsed());
            }
            other => panic!("expected prep BudgetExhausted, got {other:?}"),
        }
    }

    #[test]
    fn expiry_in_search_without_an_incumbent_stops_at_the_search_stage() {
        let base = Instant::now();
        let (_calls, now) = advancing_clock(base, 2);
        let mut cancel = || false;
        let mut clock = DeadlineClock::with_clock(
            base,
            base + std::time::Duration::from_millis(2),
            now,
            &mut cancel,
        );
        match analyze_with_clock(&[range(0, 0, 4)], 1, &mut clock) {
            Err(PlanError::BudgetExhausted { stage, timing }) => {
                assert_eq!(stage, AnalysisStage::Search);
                assert!(timing.reached());
                assert!(timing.search().is_some());
            }
            other => panic!("expected search BudgetExhausted, got {other:?}"),
        }
    }

    #[test]
    fn an_incumbent_survives_a_deadline_during_search() {
        let base = Instant::now();
        let (_calls, now) = advancing_clock(base, 3);
        let mut cancel = || false;
        let mut clock = DeadlineClock::with_clock(
            base,
            base + std::time::Duration::from_millis(3),
            now,
            &mut cancel,
        );
        let candidates = [range(0, 0, 4), range(10, 10, 3), range(20, 20, 2)];
        let plan = analyze_with_clock(&candidates, 2, &mut clock).expect("incumbent");
        assert!(!plan.ranges().is_empty());
        assert!(plan.timing().reached());
        assert!(plan.timing().search().is_some());
        assert_eq!(
            plan.timing().elapsed(),
            plan.timing().preparation() + plan.timing().search().unwrap()
        );
    }

    #[test]
    fn deadline_publishes_computed_improvement_in_the_current_row() {
        let base = Instant::now();
        let (_calls, now) = advancing_clock(base, 7);
        let mut cancel = || false;
        let mut clock = DeadlineClock::with_clock(
            base,
            base + std::time::Duration::from_millis(7),
            now,
            &mut cancel,
        );
        let candidates = [range(0, 0, 4), range(0, 0, 5), range(10, 10, 6)];
        let plan =
            analyze_with_clock(&candidates, 2, &mut clock).expect("computed plan at deadline");
        assert_eq!(plan.covered_tokens(), 10);
        assert!(plan.timing().reached());
    }

    #[test]
    fn deadline_publishes_coalesced_improvement_in_the_current_row() {
        let base = Instant::now();
        let (_calls, now) = advancing_clock(base, 7);
        let mut cancel = || false;
        let mut clock = DeadlineClock::with_clock(
            base,
            base + std::time::Duration::from_millis(7),
            now,
            &mut cancel,
        );
        let candidates = [range(0, 0, 4), range(0, 0, 5), range(4, 4, 6)];
        let plan =
            analyze_with_clock(&candidates, 2, &mut clock).expect("coalesced plan at deadline");
        assert_eq!(plan.covered_tokens(), 10);
        assert_eq!(plan.ranges(), &[range(0, 0, 10)]);
        assert!(plan.timing().reached());
    }

    #[test]
    fn cancellation_after_search_wins_even_with_an_incumbent() {
        let base = Instant::now();
        let calls = Rc::new(Cell::new(0u64));
        let cancel_calls = Rc::clone(&calls);
        let mut cancel = move || {
            cancel_calls.set(cancel_calls.get() + 1);
            cancel_calls.get() >= 10
        };
        let mut clock = DeadlineClock::with_clock(
            base,
            base + std::time::Duration::from_secs(10),
            fixed_clock(base),
            &mut cancel,
        );
        let candidates = [range(0, 0, 4), range(10, 10, 3), range(20, 20, 2)];
        match analyze_with_clock(&candidates, 2, &mut clock) {
            Err(PlanError::Cancelled { stage, timing }) => {
                assert_eq!(stage, AnalysisStage::Finish);
                assert!(!timing.reached());
                assert!(timing.search().is_some());
            }
            other => panic!("expected Finish cancellation, got {other:?}"),
        }
    }

    #[test]
    fn cancellation_at_the_deadline_with_an_incumbent_is_cancelled() {
        let base = Instant::now();
        let (_calls, now) = advancing_clock(base, 7);
        let calls = Rc::new(Cell::new(0u64));
        let cancel_calls = Rc::clone(&calls);
        let mut cancel = move || {
            cancel_calls.set(cancel_calls.get() + 1);
            cancel_calls.get() >= 9
        };
        let mut clock = DeadlineClock::with_clock(
            base,
            base + std::time::Duration::from_millis(7),
            now,
            &mut cancel,
        );
        let candidates = [range(0, 0, 4), range(0, 0, 5), range(10, 10, 6)];
        match analyze_with_clock(&candidates, 2, &mut clock) {
            Err(PlanError::Cancelled { stage, timing }) => {
                assert_eq!(stage, AnalysisStage::Search);
                assert!(timing.reached());
            }
            other => panic!("expected deadline cancellation, got {other:?}"),
        }
    }
}
