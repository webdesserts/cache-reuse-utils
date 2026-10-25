//! Absolute deadline, cancellation and returned timing metadata.
//!
//! The public entry point passes one monotonic [`Instant`] deadline through
//! preparation and search. This module's private clock is the only place that
//! reads the monotonic clock; tests inject a deterministic one.

use std::time::{Duration, Instant};

/// The stage at which an analysis stopped or failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalysisStage {
    /// Input validation and resource admission, before candidate search starts.
    Preparation,
    /// Candidate search or its bounded finishing work.
    Search,
    /// Building the returned plan after search.
    Finish,
}

/// Small timing summary for a completed plan or an informative failure.
///
/// `elapsed` runs from the analysis-start checkpoint to the finish/error
/// checkpoint. `limit` is `deadline - start` (zero when the deadline was already
/// past), `preparation` covers start to search-start (or to finish when search
/// never started), and `search` is present only when search started. `reached`
/// means the finish checkpoint was at or after the absolute deadline. These are
/// cooperative wall-clock observations, not a hard-realtime guarantee.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnalysisTiming {
    elapsed: Duration,
    limit: Duration,
    reached: bool,
    preparation: Duration,
    search: Option<Duration>,
}

impl AnalysisTiming {
    pub(crate) const fn new(
        elapsed: Duration,
        limit: Duration,
        reached: bool,
        preparation: Duration,
        search: Option<Duration>,
    ) -> Self {
        Self {
            elapsed,
            limit,
            reached,
            preparation,
            search,
        }
    }

    pub fn elapsed(&self) -> Duration {
        self.elapsed
    }

    pub fn limit(&self) -> Duration {
        self.limit
    }

    /// True when the finish checkpoint was at or after the absolute deadline.
    pub fn reached(&self) -> bool {
        self.reached
    }

    pub fn preparation(&self) -> Duration {
        self.preparation
    }

    /// Search duration, or `None` when search never started.
    pub fn search(&self) -> Option<Duration> {
        self.search
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckOutcome {
    Continue,
    DeadlineReached,
    Cancelled,
}

/// Private monotonic deadline clock. Production uses `Instant::now`; tests can
/// inject a fake clock without exposing a public clock framework.
pub(crate) struct DeadlineClock<'a> {
    started: Instant,
    deadline: Instant,
    now: Box<dyn FnMut() -> Instant + 'a>,
    cancel: &'a mut dyn FnMut() -> bool,
}

impl<'a> DeadlineClock<'a> {
    pub(crate) fn start(deadline: Instant, cancel: &'a mut dyn FnMut() -> bool) -> Self {
        Self {
            started: Instant::now(),
            deadline,
            now: Box::new(Instant::now),
            cancel,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_clock(
        started: Instant,
        deadline: Instant,
        now: impl FnMut() -> Instant + 'a,
        cancel: &'a mut dyn FnMut() -> bool,
    ) -> Self {
        Self {
            started,
            deadline,
            now: Box::new(now),
            cancel,
        }
    }

    pub(crate) fn started(&self) -> Instant {
        self.started
    }

    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Total budget from the analysis-start checkpoint, saturating at zero when
    /// the deadline was already past.
    pub(crate) fn limit(&self) -> Duration {
        self.deadline.saturating_duration_since(self.started)
    }

    pub(crate) fn now(&mut self) -> Instant {
        (self.now)()
    }

    pub(crate) fn cancelled(&mut self) -> bool {
        (self.cancel)()
    }

    /// Cancellation is checked before the deadline, so a boundary cancellation
    /// never looks like a budget result.
    pub(crate) fn check(&mut self) -> CheckOutcome {
        if (self.cancel)() {
            return CheckOutcome::Cancelled;
        }
        if (self.now)() >= self.deadline {
            return CheckOutcome::DeadlineReached;
        }
        CheckOutcome::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock_at(base: Instant, offset_ms: u64) -> impl FnMut() -> Instant {
        move || base + Duration::from_millis(offset_ms)
    }

    #[test]
    fn zero_remaining_time_is_reached_immediately_but_cancel_still_wins() {
        let base = Instant::now();
        let mut cancel = || false;
        let mut clock = DeadlineClock::with_clock(base, base, clock_at(base, 0), &mut cancel);
        assert_eq!(clock.check(), CheckOutcome::DeadlineReached);

        let mut cancelled = || true;
        let mut clock = DeadlineClock::with_clock(base, base, clock_at(base, 0), &mut cancelled);
        assert_eq!(clock.check(), CheckOutcome::Cancelled);
    }

    #[test]
    fn before_exactly_at_and_after_the_deadline_are_exact() {
        let base = Instant::now();
        let deadline = base + Duration::from_millis(5);
        let mut cancel = || false;
        for (offset_ms, expected) in [
            (0, CheckOutcome::Continue),
            (4, CheckOutcome::Continue),
            (5, CheckOutcome::DeadlineReached),
            (6, CheckOutcome::DeadlineReached),
        ] {
            let mut clock =
                DeadlineClock::with_clock(base, deadline, clock_at(base, offset_ms), &mut cancel);
            assert_eq!(clock.check(), expected, "offset {offset_ms}ms");
        }
    }

    #[test]
    fn a_deadline_before_the_start_has_zero_limit_and_is_reached() {
        let base = Instant::now();
        let deadline = base;
        let started = base + Duration::from_millis(3);
        let mut cancel = || false;
        let mut clock =
            DeadlineClock::with_clock(started, deadline, clock_at(started, 0), &mut cancel);
        assert_eq!(clock.limit(), Duration::ZERO);
        assert_eq!(clock.check(), CheckOutcome::DeadlineReached);
    }

    #[test]
    fn cancellation_at_the_actual_deadline_is_not_a_budget_stop() {
        let base = Instant::now();
        let deadline = base + Duration::from_millis(5);
        let mut cancel = || true;
        let mut clock = DeadlineClock::with_clock(base, deadline, clock_at(base, 5), &mut cancel);
        assert_eq!(clock.check(), CheckOutcome::Cancelled);
    }
}
