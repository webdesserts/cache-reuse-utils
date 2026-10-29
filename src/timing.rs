//! Deadline and cancellation checks, stage tracking and returned timing.
//!
//! [`Stopper`] is the single place analysis asks "should I stop?". Stage
//! boundaries call [`Stopper::check`]; inner loops call [`Stopper::tick`], which
//! performs a real check once every [`TICKS_PER_CHECK`] calls. Each real check
//! calls the caller's cancellation callback first and then compares
//! `Instant::now()` with the absolute deadline, so cancellation always wins.

use std::time::{Duration, Instant};

/// Inner-loop iterations between real deadline/cancellation checks. Every
/// ticked loop body is O(1), so a stop is observed within microseconds.
const TICKS_PER_CHECK: u32 = 4096;

/// The stage at which an analysis stopped or failed.
///
/// A stage can name the phase that refused entry before it started (for
/// example, `Search` when the deadline was observed after discovery). The
/// matching optional duration in [`AnalysisTiming`] is then `None`,
/// distinguishing "not entered" from a zero duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalysisStage {
    /// Input validation and resource admission, before discovery starts.
    Preparation,
    /// Exact candidate discovery, including index construction and matching.
    Discovery,
    /// Placement search over the discovered candidates.
    Search,
    /// Building the returned plan after search.
    Finish,
}

/// Small timing summary for a completed plan or an informative failure.
///
/// `elapsed` runs from the analysis start to the finish or error checkpoint.
/// `limit` is `deadline - start` (zero when the deadline had already passed).
/// `preparation` covers start to discovery start (or to finish when discovery
/// never started). `discovery` and `search` are `None` when that stage was not
/// entered. Stage boundaries share checkpoints, so
/// `preparation + discovery + search == elapsed` for the entered stages.
/// `reached` means the finish checkpoint was at or after the deadline. These
/// are cooperative wall-clock observations, not a hard-realtime guarantee.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnalysisTiming {
    elapsed: Duration,
    limit: Duration,
    preparation: Duration,
    discovery: Option<Duration>,
    search: Option<Duration>,
}

impl AnalysisTiming {
    pub fn elapsed(&self) -> Duration {
        self.elapsed
    }

    pub fn limit(&self) -> Duration {
        self.limit
    }

    /// True when the finish checkpoint was at or after the absolute deadline.
    pub fn reached(&self) -> bool {
        self.elapsed >= self.limit
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

/// Why a check asked analysis to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stop {
    Cancelled,
    Deadline,
}

/// Cooperative deadline/cancellation checks plus the stage boundaries used to
/// label errors and build [`AnalysisTiming`].
pub(crate) struct Stopper<'a> {
    started: Instant,
    deadline: Instant,
    cancel: &'a mut dyn FnMut() -> bool,
    ticks: u32,
    ticks_per_check: u32,
    stage: AnalysisStage,
    discovery_start: Option<Instant>,
    discovery_end: Option<Instant>,
    search_start: Option<Instant>,
    #[cfg(test)]
    clock: Option<Box<dyn FnMut(AnalysisStage) -> Instant + 'a>>,
}

impl<'a> Stopper<'a> {
    pub(crate) fn new(deadline: Instant, cancel: &'a mut dyn FnMut() -> bool) -> Self {
        Self {
            started: Instant::now(),
            deadline,
            cancel,
            ticks: 0,
            ticks_per_check: TICKS_PER_CHECK,
            stage: AnalysisStage::Preparation,
            discovery_start: None,
            discovery_end: None,
            search_start: None,
            #[cfg(test)]
            clock: None,
        }
    }

    /// Test seam: time comes from `clock`, which sees the current stage, and
    /// every tick performs a real check. Time starts at `clock(Preparation)`.
    #[cfg(test)]
    pub(crate) fn with_clock(
        deadline: Instant,
        cancel: &'a mut dyn FnMut() -> bool,
        mut clock: impl FnMut(AnalysisStage) -> Instant + 'a,
    ) -> Self {
        let started = clock(AnalysisStage::Preparation);
        Self {
            started,
            ticks_per_check: 1,
            clock: Some(Box::new(clock)),
            ..Self::new(deadline, cancel)
        }
    }

    pub(crate) fn now(&mut self) -> Instant {
        #[cfg(test)]
        if let Some(clock) = &mut self.clock {
            return clock(self.stage);
        }
        Instant::now()
    }

    pub(crate) fn stage(&self) -> AnalysisStage {
        self.stage
    }

    pub(crate) fn cancelled(&mut self) -> bool {
        (self.cancel)()
    }

    /// Check cancellation, then the deadline.
    pub(crate) fn check(&mut self) -> Result<(), Stop> {
        if self.cancelled() {
            return Err(Stop::Cancelled);
        }
        if self.now() >= self.deadline {
            return Err(Stop::Deadline);
        }
        Ok(())
    }

    /// Inner-loop check: a real [`Self::check`] once per `TICKS_PER_CHECK`.
    #[inline]
    pub(crate) fn tick(&mut self) -> Result<(), Stop> {
        self.ticks += 1;
        if self.ticks < self.ticks_per_check {
            return Ok(());
        }
        self.ticks = 0;
        self.check()
    }

    pub(crate) fn enter_discovery(&mut self) {
        self.stage = AnalysisStage::Discovery;
        self.discovery_start = Some(self.now());
    }

    /// Close discovery and label later stops as `Search`. Search timing starts
    /// only at [`Self::start_search`], so a refusal at this boundary reports no
    /// search duration.
    pub(crate) fn finish_discovery(&mut self) {
        self.discovery_end = Some(self.now());
        self.stage = AnalysisStage::Search;
    }

    pub(crate) fn start_search(&mut self) {
        self.search_start = self.discovery_end;
    }

    pub(crate) fn enter_finish(&mut self) {
        self.stage = AnalysisStage::Finish;
    }

    /// Timing up to now, using the recorded stage boundaries.
    pub(crate) fn timing(&mut self) -> AnalysisTiming {
        let finish = self.now();
        let started = self.started;
        let discovery_start = self.discovery_start.unwrap_or(finish);
        AnalysisTiming {
            elapsed: finish.saturating_duration_since(started),
            limit: self.deadline.saturating_duration_since(started),
            preparation: discovery_start.saturating_duration_since(started),
            discovery: self.discovery_start.map(|start| {
                self.discovery_end
                    .unwrap_or(finish)
                    .saturating_duration_since(start)
            }),
            search: self
                .search_start
                .map(|start| finish.saturating_duration_since(start)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_is_checked_before_an_expired_deadline() {
        let base = Instant::now();
        let mut cancelled = || true;
        let mut stopper = Stopper::with_clock(base, &mut cancelled, move |_| base);
        assert_eq!(stopper.check(), Err(Stop::Cancelled));

        let mut running = || false;
        let mut stopper = Stopper::with_clock(base, &mut running, move |_| base);
        assert_eq!(stopper.check(), Err(Stop::Deadline));
    }

    #[test]
    fn tick_checks_once_per_interval() {
        let mut calls = 0u32;
        let mut cancel = || {
            calls += 1;
            false
        };
        let mut stopper = Stopper::new(Instant::now() + Duration::from_secs(60), &mut cancel);
        for _ in 0..TICKS_PER_CHECK * 3 {
            stopper.tick().expect("running");
        }
        drop(stopper);
        assert_eq!(calls, 3);
    }

    #[test]
    fn a_past_deadline_has_zero_limit() {
        let base = Instant::now();
        let mut cancel = || false;
        let mut stopper =
            Stopper::with_clock(base, &mut cancel, move |_| base + Duration::from_millis(3));
        let timing = stopper.timing();
        assert_eq!(timing.limit(), Duration::ZERO);
        assert!(timing.reached());
    }
}
