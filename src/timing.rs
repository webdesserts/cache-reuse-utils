//! Absolute deadline, cancellation and returned timing metadata.
//!
//! The public entry point registers one monotonic [`Instant`] deadline with a
//! shared background timer owner. Discovery/search read that analysis's
//! notification flag at natural loop points; this module's clock reads remain at
//! phase boundaries for telemetry. Tests can inject deterministic time and a
//! manual expiry signal.

use std::time::{Duration, Instant};

use crate::deadline::{self, DeadlineRegistration, DeadlineSignal, TimerError};

/// The stage at which an analysis stopped or failed.
///
/// A stage can identify the phase that refused entry before it started (for
/// example, `Search` when discovery completed at the deadline). The matching
/// optional timing is then `None`, distinguishing “not entered” from a zero
/// duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalysisStage {
    /// Input validation and resource admission, before discovery starts.
    Preparation,
    /// Exact candidate discovery, including index construction and matching.
    Discovery,
    /// Candidate search or its bounded finishing work.
    Search,
    /// Building the returned plan after search.
    Finish,
}

/// Small timing summary for a completed plan or an informative failure.
///
/// `elapsed` runs from the analysis-start checkpoint to the finish/error
/// checkpoint. `limit` is `deadline - start` (zero when the deadline was already
/// past). `preparation` covers start to discovery-start (or finish when
/// discovery never started), `discovery` is present when discovery ran, and
/// `search` is present only when search started. Stage boundaries share the
/// finish checkpoint convention, so at a successful finish
/// `preparation + discovery + search == elapsed`; absent stages are `None`.
/// `reached` means the finish checkpoint was at or after the absolute deadline.
/// These are cooperative wall-clock observations, not a hard-realtime guarantee.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnalysisTiming {
    elapsed: Duration,
    limit: Duration,
    reached: bool,
    preparation: Duration,
    discovery: Option<Duration>,
    search: Option<Duration>,
}

impl AnalysisTiming {
    pub(crate) const fn new(
        elapsed: Duration,
        limit: Duration,
        reached: bool,
        preparation: Duration,
        discovery: Option<Duration>,
        search: Option<Duration>,
    ) -> Self {
        Self {
            elapsed,
            limit,
            reached,
            preparation,
            discovery,
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

    /// Discovery duration, or `None` when discovery never started.
    pub fn discovery(&self) -> Option<Duration> {
        self.discovery
    }

    /// Search duration, or `None` when search never started.
    pub fn search(&self) -> Option<Duration> {
        self.search
    }
}

/// The single production clock read. Feature-enabled but unarmed threads and
/// default builds use the real standard clock.
fn now_std() -> Instant {
    #[cfg(feature = "test-support")]
    if let Some(now) = crate::test_support::mapped_now() {
        return now;
    }
    Instant::now()
}

/// Concrete stage-boundary state owned by timing. The orchestrator records the
/// stages it actually entered; finish() turns those points into public timing.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TimingState {
    discovery_start: Instant,
    discovery_end: Option<Instant>,
    search_start: Option<Instant>,
}

impl TimingState {
    pub(crate) fn new(discovery_start: Instant) -> Self {
        Self {
            discovery_start,
            discovery_end: None,
            search_start: None,
        }
    }

    pub(crate) fn discovery_finished(&mut self, end: Instant) {
        self.discovery_end = Some(end);
    }

    pub(crate) fn search_started(&mut self, start: Instant) {
        self.search_start = Some(start);
    }

    pub(crate) fn finish(&self, clock: &DeadlineClock<'_>, finish: Instant) -> AnalysisTiming {
        AnalysisTiming::from_stages(
            clock,
            finish,
            self.discovery_start,
            self.discovery_end,
            self.search_start,
        )
    }
}

impl AnalysisTiming {
    /// Timing for a failure or no-work result before discovery starts.
    pub(crate) fn preparation_only(clock: &DeadlineClock<'_>, finish: Instant) -> Self {
        Self::from_stages(clock, finish, finish, None, None)
    }

    fn from_stages(
        clock: &DeadlineClock<'_>,
        finish: Instant,
        discovery_start: Instant,
        discovery_end: Option<Instant>,
        search_start: Option<Instant>,
    ) -> Self {
        let started = clock.started();
        let elapsed = finish.saturating_duration_since(started);
        let preparation = discovery_start.saturating_duration_since(started);
        let discovery = discovery_end.map(|end| end.saturating_duration_since(discovery_start));
        let search = search_start.map(|start| finish.saturating_duration_since(start));
        Self::new(
            elapsed,
            clock.limit(),
            finish >= clock.deadline(),
            preparation,
            discovery,
            search,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckOutcome {
    Continue,
    DeadlineReached,
    Cancelled,
}

/// Private deadline clock. The background timer flag is the control authority;
/// `now` is used only for phase-boundary telemetry. Tests can inject a clock and
/// a signal without exposing a public clock framework.
pub(crate) struct DeadlineClock<'a> {
    started: Instant,
    deadline: Instant,
    now: Box<dyn FnMut() -> Instant + 'a>,
    cancel: &'a mut dyn FnMut() -> bool,
    signal: DeadlineSignal,
    _registration: Option<DeadlineRegistration>,
    #[cfg(feature = "test-support")]
    manual_binding: Option<crate::test_support::ManualDeadlineBinding>,
}

impl<'a> DeadlineClock<'a> {
    pub(crate) fn start(deadline: Instant, cancel: &'a mut dyn FnMut() -> bool) -> Self {
        Self {
            started: now_std(),
            deadline,
            now: Box::new(now_std),
            cancel,
            signal: DeadlineSignal::new(),
            _registration: None,
            #[cfg(feature = "test-support")]
            manual_binding: None,
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
            signal: DeadlineSignal::new(),
            _registration: None,
            #[cfg(feature = "test-support")]
            manual_binding: None,
        }
    }

    /// Start the background owner after validation/no-work handling. A due
    /// deadline initializes the signal directly; future deadlines register with
    /// the shared service.
    pub(crate) fn arm_timer(&mut self) -> Result<(), TimerError> {
        #[cfg(feature = "test-support")]
        if let Some(binding) = crate::test_support::ManualDeadlineBinding::install() {
            self.signal = binding.signal();
            self.manual_binding = Some(binding);
            // Match production registration: compare the current bridged time,
            // not the analysis start checkpoint.
            if self.now() >= self.deadline {
                self.signal.expire();
            }
            return Ok(());
        }
        self._registration = Some(deadline::register(self.deadline, self.signal.clone())?);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn expire_for_test(&self) {
        self.signal.expire();
    }

    #[cfg(test)]
    pub(crate) fn signal_for_test(&self) -> DeadlineSignal {
        self.signal.clone()
    }

    #[cfg(test)]
    pub(crate) fn with_clock_signal(
        started: Instant,
        deadline: Instant,
        now: impl FnMut() -> Instant + 'a,
        signal: DeadlineSignal,
        cancel: &'a mut dyn FnMut() -> bool,
    ) -> Self {
        Self {
            started,
            deadline,
            now: Box::new(now),
            cancel,
            signal,
            _registration: None,
            #[cfg(feature = "test-support")]
            manual_binding: None,
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
        if self.signal.expired() {
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
    fn zero_remaining_time_is_reached_by_the_signal_but_cancel_still_wins() {
        let base = Instant::now();
        let mut cancel = || false;
        let mut clock = DeadlineClock::with_clock(base, base, clock_at(base, 0), &mut cancel);
        clock.expire_for_test();
        assert_eq!(clock.check(), CheckOutcome::DeadlineReached);

        let mut cancelled = || true;
        let mut clock = DeadlineClock::with_clock(base, base, clock_at(base, 0), &mut cancelled);
        clock.expire_for_test();
        assert_eq!(clock.check(), CheckOutcome::Cancelled);
    }

    #[test]
    fn signal_expiry_is_the_control_and_cancellation_precedes_it() {
        let base = Instant::now();
        let deadline = base + Duration::from_secs(5);
        let mut cancel = || false;
        let mut clock = DeadlineClock::with_clock(base, deadline, clock_at(base, 0), &mut cancel);
        assert_eq!(clock.check(), CheckOutcome::Continue);
        clock.expire_for_test();
        assert_eq!(clock.check(), CheckOutcome::DeadlineReached);

        let mut cancelled = || true;
        let mut clock =
            DeadlineClock::with_clock(base, deadline, clock_at(base, 0), &mut cancelled);
        clock.expire_for_test();
        assert_eq!(clock.check(), CheckOutcome::Cancelled);
    }

    #[test]
    fn a_deadline_before_the_start_has_zero_limit_and_signal_expiry() {
        let base = Instant::now();
        let deadline = base;
        let started = base + Duration::from_millis(3);
        let mut cancel = || false;
        let mut clock =
            DeadlineClock::with_clock(started, deadline, clock_at(started, 0), &mut cancel);
        assert_eq!(clock.limit(), Duration::ZERO);
        clock.expire_for_test();
        assert_eq!(clock.check(), CheckOutcome::DeadlineReached);
    }

    #[test]
    fn cancellation_at_an_expired_signal_is_not_a_budget_stop() {
        let base = Instant::now();
        let deadline = base + Duration::from_millis(5);
        let mut cancel = || true;
        let mut clock = DeadlineClock::with_clock(base, deadline, clock_at(base, 5), &mut cancel);
        clock.expire_for_test();
        assert_eq!(clock.check(), CheckOutcome::Cancelled);
    }
    #[test]
    fn real_timer_delivery_reaches_the_clock_control() {
        let deadline = Instant::now() + Duration::from_millis(250);
        let mut cancel = || false;
        let mut clock = DeadlineClock::start(deadline, &mut cancel);
        clock.arm_timer().expect("timer");
        let signal = clock.signal_for_test();
        assert!(!signal.expired(), "future deadline must not be pre-expired");
        signal.wait_for_expiry(Duration::from_secs(5));
        assert_eq!(clock.check(), CheckOutcome::DeadlineReached);
    }
}
