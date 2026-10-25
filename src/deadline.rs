//! Private background deadline ownership.
//!
//! One lazily started process-local scheduler owns registrations. Each analysis
//! gets a unique notification flag; the scheduler is the only production writer.
//! The flag carries no data, so relaxed atomics are sufficient. Cancellation is a
//! separate caller callback and is checked before the flag.

use std::collections::BTreeMap;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::thread;
#[cfg(test)]
use std::time::Duration;
use std::time::Instant;

/// Per-analysis single-writer/multiple-reader deadline notification.
#[derive(Clone)]
pub(crate) struct DeadlineSignal {
    flag: Arc<AtomicBool>,
    #[cfg(test)]
    wait: Arc<(Mutex<()>, Condvar)>,
}

impl DeadlineSignal {
    pub(crate) fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            wait: Arc::new((Mutex::new(()), Condvar::new())),
        }
    }

    pub(crate) fn expired(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }

    pub(crate) fn expire(&self) {
        self.flag.store(true, Ordering::Relaxed);
        #[cfg(test)]
        {
            let (lock, wake) = &*self.wait;
            let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            wake.notify_all();
        }
    }

    #[cfg(test)]
    pub(crate) fn wait_for_expiry(&self, watchdog: Duration) {
        if self.expired() {
            return;
        }
        let (lock, wake) = &*self.wait;
        let mut guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let started = Instant::now();
        while !self.expired() {
            let remaining = watchdog.saturating_sub(started.elapsed());
            assert!(!remaining.is_zero(), "timer did not expire within watchdog");
            let (next, _) = wake
                .wait_timeout(guard, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            guard = next;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimerError {
    Start,
    Poisoned,
}

type Key = (Instant, u64);

struct State {
    entries: BTreeMap<Key, DeadlineSignal>,
    next_id: u64,
    running: bool,
    #[cfg(test)]
    waiting: bool,
}

/// A registration must be dropped as soon as analysis finishes. Drop only
/// removes its own key and wakes the scheduler; it never panics.
pub(crate) struct DeadlineRegistration {
    service: Option<Arc<TimerService>>,
    key: Key,
}

impl Drop for DeadlineRegistration {
    fn drop(&mut self) {
        if let Some(service) = &self.service {
            service.cancel(self.key);
        }
    }
}

struct TimerService {
    state: Mutex<State>,
    wake: Condvar,
}

impl TimerService {
    fn new() -> Self {
        Self {
            state: Mutex::new(State {
                entries: BTreeMap::new(),
                next_id: 0,
                running: false,
                #[cfg(test)]
                waiting: false,
            }),
            wake: Condvar::new(),
        }
    }

    fn register(
        self: &Arc<Self>,
        deadline: Instant,
        signal: DeadlineSignal,
    ) -> Result<DeadlineRegistration, TimerError> {
        self.register_with_spawn(deadline, signal, |service| {
            thread::Builder::new()
                .name("scr-deadline".into())
                .spawn(move || run_worker(service))
                .map(|_| ())
        })
    }

    fn register_with_spawn<F>(
        self: &Arc<Self>,
        deadline: Instant,
        signal: DeadlineSignal,
        spawn: F,
    ) -> Result<DeadlineRegistration, TimerError>
    where
        F: FnOnce(Arc<TimerService>) -> io::Result<()>,
    {
        let now = Instant::now();
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => return Err(TimerError::Poisoned),
        };
        if deadline <= now {
            // Due before publication: initialize the flag directly. No worker
            // is needed and there is no later writer for this registration.
            signal.expire();
            return Ok(DeadlineRegistration {
                service: None,
                key: (deadline, 0),
            });
        }
        let id = state.next_id;
        state.next_id = state.next_id.wrapping_add(1);
        let key = (deadline, id);
        state.entries.insert(key, signal);
        if !state.running {
            if spawn(Arc::clone(self)).is_err() {
                state.entries.remove(&key);
                return Err(TimerError::Start);
            }
            state.running = true;
        } else {
            self.wake.notify_all();
        }
        Ok(DeadlineRegistration {
            service: Some(Arc::clone(self)),
            key,
        })
    }

    fn cancel(&self, key: Key) {
        let mut state = lock_recover(&self.state);
        state.entries.remove(&key);
        self.wake.notify_all();
    }

    fn fail_pending(&self) {
        let mut state = lock_recover(&self.state);
        // A service/worker failure forces pending signals so analyses stop.
        // This is not a typed timer-failure propagation for already-running
        // calls: they may report BudgetExhausted or return a prior incumbent.
        for signal in state.entries.values() {
            signal.expire();
        }
        state.entries.clear();
        state.running = false;
    }
}

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn run_worker(service: Arc<TimerService>) {
    let result = catch_unwind(AssertUnwindSafe(|| worker_loop(&service)));
    if result.is_err() {
        // Do not leave registered analyses without a deadline notification.
        service.fail_pending();
    }
}

fn worker_loop(service: &Arc<TimerService>) {
    let mut state = match service.state.lock() {
        Ok(state) => state,
        Err(poisoned) => {
            let mut state = poisoned.into_inner();
            for signal in state.entries.values() {
                signal.expire();
            }
            state.entries.clear();
            state.running = false;
            return;
        }
    };
    loop {
        let now = Instant::now();
        while let Some((&key, _)) = state.entries.first_key_value() {
            if key.0 > now {
                break;
            }
            let signal = state.entries.remove(&key).expect("first entry exists");
            signal.expire();
        }
        if state.entries.is_empty() {
            state.running = false;
            service.wake.notify_all();
            return;
        }
        let next = state
            .entries
            .first_key_value()
            .expect("nonempty registry")
            .0
            .0;
        let timeout = next.saturating_duration_since(now);
        #[cfg(test)]
        {
            state.waiting = true;
            service.wake.notify_all();
        }
        let (next_state, _) = service
            .wake
            .wait_timeout(state, timeout)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state = next_state;
        #[cfg(test)]
        {
            state.waiting = false;
        }
    }
}

static SERVICE: OnceLock<Arc<TimerService>> = OnceLock::new();

#[cfg(test)]
thread_local! {
    static FORCE_START_FAILURE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn fail_next_registration() {
    FORCE_START_FAILURE.with(|force| force.set(true));
}

fn service() -> Arc<TimerService> {
    SERVICE
        .get_or_init(|| Arc::new(TimerService::new()))
        .clone()
}

pub(crate) fn register(
    deadline: Instant,
    signal: DeadlineSignal,
) -> Result<DeadlineRegistration, TimerError> {
    #[cfg(test)]
    if FORCE_START_FAILURE.with(std::cell::Cell::take) {
        return Err(TimerError::Start);
    }
    service().register(deadline, signal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
    use std::sync::mpsc;
    use std::time::Duration;

    fn wait_for_expiry(signal: &DeadlineSignal, watchdog: Duration) {
        signal.wait_for_expiry(watchdog);
    }

    #[test]
    fn due_now_is_set_at_registration_without_a_worker() {
        let service = Arc::new(TimerService::new());
        let signal = DeadlineSignal::new();
        let _registration = service
            .register(Instant::now() - Duration::from_millis(1), signal.clone())
            .expect("due registration");
        assert!(signal.expired());
    }

    #[test]
    fn worker_delivers_future_deadline() {
        let service = Arc::new(TimerService::new());
        let signal = DeadlineSignal::new();
        let registration = service
            .register(Instant::now() + Duration::from_millis(50), signal.clone())
            .expect("registration");
        wait_for_expiry(&signal, Duration::from_secs(5));
        drop(registration);

        let second = DeadlineSignal::new();
        let _registration = service
            .register(Instant::now() + Duration::from_secs(60), second.clone())
            .expect("second registration");
        // Removing the first registration must not leave a stale flag.
        assert!(signal.expired());
        assert!(!second.expired());
    }

    fn wait_until_waiting(service: &Arc<TimerService>) {
        let mut state = lock_recover(&service.state);
        let started = Instant::now();
        while !state.waiting {
            let remaining = Duration::from_secs(5).saturating_sub(started.elapsed());
            assert!(!remaining.is_zero(), "worker did not enter timed wait");
            let (next, _) = service
                .wake
                .wait_timeout(state, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next;
        }
    }

    #[test]
    fn earlier_registration_wakes_the_far_future_wait() {
        let service = Arc::new(TimerService::new());
        let far = DeadlineSignal::new();
        let _far_registration = service
            .register(Instant::now() + Duration::from_secs(3600), far.clone())
            .expect("far");
        wait_until_waiting(&service);
        let near = DeadlineSignal::new();
        let _near_registration = service
            .register(Instant::now() + Duration::from_millis(200), near.clone())
            .expect("near");
        assert!(!near.expired());
        wait_for_expiry(&near, Duration::from_secs(5));
        assert!(!far.expired());
    }

    #[test]
    fn drop_before_due_removes_registration() {
        let service = Arc::new(TimerService::new());
        let signal = DeadlineSignal::new();
        let registration = service
            .register(Instant::now() + Duration::from_secs(60), signal.clone())
            .expect("registration");
        assert_eq!(lock_recover(&service.state).entries.len(), 1);
        drop(registration);
        assert!(lock_recover(&service.state).entries.is_empty());
        assert!(!signal.expired());
    }

    #[test]
    fn spawn_failure_is_typed_and_removes_entry() {
        let service = Arc::new(TimerService::new());
        let signal = DeadlineSignal::new();
        let error = match service.register_with_spawn(
            Instant::now() + Duration::from_secs(60),
            signal.clone(),
            |_| Err(io::Error::other("injected spawn failure")),
        ) {
            Ok(_) => panic!("spawn should fail"),
            Err(error) => error,
        };
        assert_eq!(error, TimerError::Start);
        assert!(!signal.expired());
        let state = lock_recover(&service.state);
        assert!(state.entries.is_empty());
        assert!(!state.running);
    }

    #[test]
    fn poisoned_service_fails_closed_without_panicking_drop() {
        let service = Arc::new(TimerService::new());
        let poison = Arc::clone(&service);
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _guard = poison.state.lock().expect("unpoisoned");
            panic!("poison service for test");
        }));
        assert!(result.is_err());
        let signal = DeadlineSignal::new();
        let error = match service.register(Instant::now() + Duration::from_secs(60), signal.clone())
        {
            Ok(_) => panic!("poisoned service must reject registration"),
            Err(error) => error,
        };
        assert_eq!(error, TimerError::Poisoned);
        assert!(!signal.expired());
    }

    #[test]
    fn independent_signals_and_parallel_registrations_do_not_cross() {
        let service = Arc::new(TimerService::new());
        let left = DeadlineSignal::new();
        let right = DeadlineSignal::new();
        let _left = service
            .register(Instant::now() + Duration::from_millis(20), left.clone())
            .expect("left");
        let _right = service
            .register(Instant::now() + Duration::from_secs(60), right.clone())
            .expect("right");
        wait_for_expiry(&left, Duration::from_secs(5));
        assert!(!right.expired());
    }

    #[test]
    fn concurrent_registrations_reach_only_their_signals() {
        let service = Arc::new(TimerService::new());
        let barrier = Arc::new(Barrier::new(2));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let service = Arc::clone(&service);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                let signal = DeadlineSignal::new();
                let _registration = service
                    .register(Instant::now() + Duration::from_millis(20), signal.clone())
                    .expect("registration");
                let (tx, rx) = mpsc::channel();
                let watch_signal = signal.clone();
                let watcher = thread::spawn(move || {
                    wait_for_expiry(&watch_signal, Duration::from_secs(5));
                    let _ = tx.send(());
                });
                barrier.wait();
                rx.recv_timeout(Duration::from_secs(5)).expect("timer");
                watcher.join().expect("watcher");
                drop(_registration);
            }));
        }
        for handle in handles {
            handle.join().expect("registration thread");
        }
    }

    #[test]
    fn idle_worker_can_be_restarted() {
        let service = Arc::new(TimerService::new());
        let first = DeadlineSignal::new();
        let _first = service
            .register(Instant::now() + Duration::from_millis(20), first.clone())
            .expect("first");
        wait_for_expiry(&first, Duration::from_secs(5));
        // Wait on the service condition rather than polling/sleeping.
        let mut state = lock_recover(&service.state);
        let start = Instant::now();
        while state.running {
            let remaining = Duration::from_secs(5).saturating_sub(start.elapsed());
            assert!(!remaining.is_zero(), "worker did not idle");
            let (next, _) = service
                .wake
                .wait_timeout(state, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next;
        }
        drop(state);
        let second = DeadlineSignal::new();
        let _second = service
            .register(Instant::now() + Duration::from_millis(20), second.clone())
            .expect("restart");
        wait_for_expiry(&second, Duration::from_secs(5));
    }
}
