//! Opt-in clock support for public integration tests.
//!
//! This module exists only with the `test-support` feature. It is an
//! experimental testing surface, not part of the normal runtime contract.
//! Production and feature-enabled but unarmed threads continue to read
//! `std::time::Instant::now()`.
//!
//! A [`MockClockGuard`] installs a per-thread mapping from the thread-local
//! `mock_instant` elapsed duration onto one real `std::time::Instant` anchor.
//! The mapping is reentrancy-safe and removed by RAII. Callers should construct
//! their public deadlines with [`MockClockGuard::deadline_after`] while the
//! guard is active; the ordinary `CacheReuseAnalysis::analyze` API receives an
//! ordinary `std::time::Instant`.
//!
//! This does not make virtual time advance during work. A test that needs a
//! deadline notification calls [`MockClockGuard::expire_deadline`] from the
//! existing cancellation callback (returning `false` unless testing
//! cancellation); this is an explicit simulated timer delivery, not an OS timer
//! wake. Virtual time can be advanced separately for telemetry. Exact checkpoint
//! phase targeting remains coupled to the test's poll count. Extra `now()` reads
//! do not advance virtual time.
//!
//! While armed, callers must advance only through [`MockClockGuard::advance`].
//! Direct mutation, reset or rewinding of the underlying `mock_instant` clock is
//! unsupported and is not detected by this guard. `deadline_after` returns
//! [`MockClockError`] on domain overflow. If virtual elapsed time exceeds the
//! `std::time::Instant` range during analysis, the clock bridge panics; advancing
//! the mock `Duration` itself can also panic on `Duration` overflow. These
//! failures are deliberately loud in test support.

use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use mock_instant::thread_local::MockClock;

use crate::deadline::DeadlineSignal;

#[derive(Clone, Copy)]
struct Bridge {
    anchor: Instant,
}

thread_local! {
    static BRIDGE: RefCell<Option<Bridge>> = const { RefCell::new(None) };
    static CURRENT_SIGNAL: RefCell<Option<DeadlineSignal>> = const { RefCell::new(None) };
}

/// Failure to create or extend a mock-clock guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MockClockError {
    reason: &'static str,
}

impl MockClockError {
    const NESTED: Self = Self {
        reason: "a mock clock guard is already installed on this thread",
    };
    const OVERFLOW: Self = Self {
        reason: "mock clock duration overflowed the std::time::Instant domain",
    };
}

impl std::fmt::Display for MockClockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.reason)
    }
}

impl std::error::Error for MockClockError {}

fn bridge() -> Option<Bridge> {
    BRIDGE.with(|slot| *slot.borrow())
}

/// Scoped binding for one analysis's manual deadline signal. Drop restores the
/// previous binding, so nested analyses restore the outer signal and a finished
/// outer analysis leaves no active binding behind.
pub(crate) struct ManualDeadlineBinding {
    signal: DeadlineSignal,
    previous: Option<DeadlineSignal>,
}

impl ManualDeadlineBinding {
    pub(crate) fn install() -> Option<Self> {
        bridge()?;
        let signal = DeadlineSignal::new();
        let previous = CURRENT_SIGNAL.with(|slot| slot.replace(Some(signal.clone())));
        Some(Self { signal, previous })
    }

    pub(crate) fn signal(&self) -> DeadlineSignal {
        self.signal.clone()
    }
}

impl Drop for ManualDeadlineBinding {
    fn drop(&mut self) {
        let previous = self.previous.take();
        CURRENT_SIGNAL.with(|slot| *slot.borrow_mut() = previous);
    }
}

/// Central feature-gated time mapping used by the private clock seam.
pub(crate) fn mapped_now() -> Option<Instant> {
    let bridge = bridge()?;
    Some(
        bridge
            .anchor
            .checked_add(MockClock::time())
            .expect("mock elapsed time overflowed std::time::Instant"),
    )
}

/// Per-thread mock clock guard for public integration tests.
///
/// The guard is neither `Send` nor `Sync`; create it on the test thread, use it
/// there, and let its `Drop` restore real time. Nested guards are rejected
/// without changing the active guard. Drop clears the bridge and resets the
/// thread-local mock clock, including during panic unwinding.
#[must_use = "the guard controls the thread-local mock clock"]
pub struct MockClockGuard {
    thread_id: ThreadId,
    anchor: Instant,
    _not_send: PhantomData<Rc<()>>,
}

impl MockClockGuard {
    /// Install a fresh thread-local mock clock and bridge.
    pub fn new() -> Result<Self, MockClockError> {
        if bridge().is_some() {
            return Err(MockClockError::NESTED);
        }
        MockClock::set_time(Duration::ZERO);
        let anchor = Instant::now();
        BRIDGE.with(|slot| *slot.borrow_mut() = Some(Bridge { anchor }));
        CURRENT_SIGNAL.with(|slot| *slot.borrow_mut() = None);
        Ok(Self {
            thread_id: std::thread::current().id(),
            anchor,
            _not_send: PhantomData,
        })
    }

    /// Advance the thread-local mock clock.
    pub fn advance(&self, duration: Duration) {
        debug_assert_eq!(self.thread_id, std::thread::current().id());
        MockClock::advance(duration);
    }

    /// Current virtual elapsed time since guard entry.
    pub fn elapsed(&self) -> Duration {
        debug_assert_eq!(self.thread_id, std::thread::current().id());
        MockClock::time()
    }

    /// Simulate delivery of the current analysis deadline flag. This is an
    /// explicit timer notification, not a wall-clock sleep.
    pub fn expire_deadline(&self) -> bool {
        debug_assert_eq!(self.thread_id, std::thread::current().id());
        CURRENT_SIGNAL.with(|slot| {
            slot.borrow().as_ref().is_some_and(|signal| {
                signal.expire();
                true
            })
        })
    }

    /// Build an absolute std deadline in the same bridge domain.
    pub fn deadline_after(&self, budget: Duration) -> Result<Instant, MockClockError> {
        debug_assert_eq!(self.thread_id, std::thread::current().id());
        let elapsed = self.elapsed();
        let target = elapsed
            .checked_add(budget)
            .ok_or(MockClockError::OVERFLOW)?;
        self.anchor
            .checked_add(target)
            .ok_or(MockClockError::OVERFLOW)
    }
}

impl Drop for MockClockGuard {
    fn drop(&mut self) {
        if self.thread_id == std::thread::current().id() {
            BRIDGE.with(|slot| *slot.borrow_mut() = None);
            CURRENT_SIGNAL.with(|slot| *slot.borrow_mut() = None);
            MockClock::set_time(Duration::ZERO);
        }
    }
}
