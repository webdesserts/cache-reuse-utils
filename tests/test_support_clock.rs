#![cfg(feature = "test-support")]

//! Public integration tests for the opt-in clock bridge. These compile only
//! when `test-support` is enabled and exercise the ordinary public API.

use cache_reuse_utils::test_support::MockClockGuard;
use cache_reuse_utils::{
    AnalysisStage, CacheIdentity, CacheReuseAnalysis, CacheView, PlacementPolicy, PlanError,
    ReusePlan,
};
use mock_instant::thread_local::MockClock;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

fn view<'a>(tokens: &'a [u32]) -> CacheView<'a> {
    CacheView {
        tokens,
        identity: CacheIdentity::new(0xabc),
        minimum_match_tokens: 2,
    }
}

fn analyze(
    context: &[u32],
    cache_tokens: &[u32],
    k: u32,
    deadline: Instant,
    cancel: &mut dyn FnMut() -> bool,
) -> Result<ReusePlan, PlanError> {
    CacheReuseAnalysis {
        context_tokens: context,
        cache: view(cache_tokens),
        k,
        deadline,
        cancel,
        placement: PlacementPolicy::without_padding(context.len()),
    }
    .analyze()
}

#[test]
fn armed_clock_public_analysis_reports_phase_durations() {
    let clock = MockClockGuard::new().expect("guard");
    let deadline = clock
        .deadline_after(Duration::from_secs(10))
        .expect("deadline");
    let mut cancel = || {
        clock.advance(Duration::from_millis(1));
        false
    };
    let plan = analyze(
        &[1, 2, 3, 4, 5],
        &[9, 1, 2, 3, 4, 5, 9],
        1,
        deadline,
        &mut cancel,
    )
    .expect("plan");
    let timing = plan.timing();
    assert!(timing.discovery().expect("discovery") > Duration::ZERO);
    assert!(timing.search().expect("search") > Duration::ZERO);
    assert_eq!(
        timing.elapsed(),
        timing.preparation() + timing.discovery().unwrap() + timing.search().unwrap()
    );
}

#[test]
fn extra_clock_reads_do_not_advance_virtual_time() {
    let clock = MockClockGuard::new().expect("guard");
    let deadline = clock
        .deadline_after(Duration::from_secs(10))
        .expect("deadline");
    let mut cancel = || false;
    let plan = analyze(
        &[1, 2, 3, 4, 5],
        &[9, 1, 2, 3, 4, 5, 9],
        1,
        deadline,
        &mut cancel,
    )
    .expect("plan");
    assert_eq!(clock.elapsed(), Duration::ZERO);
    assert_eq!(plan.timing().discovery(), Some(Duration::ZERO));
    assert_eq!(plan.timing().search(), Some(Duration::ZERO));
}

#[test]
fn preparation_deadline_is_reported_without_later_stages() {
    let clock = MockClockGuard::new().expect("guard");
    let deadline = clock.deadline_after(Duration::ZERO).expect("deadline");
    let mut cancel = || false;
    match analyze(&[1, 2, 3], &[1, 2, 3], 1, deadline, &mut cancel) {
        Err(PlanError::BudgetExhausted { stage, timing }) => {
            assert_eq!(stage, AnalysisStage::Preparation);
            assert!(timing.discovery().is_none());
            assert!(timing.search().is_none());
        }
        other => panic!("expected preparation budget error, got {other:?}"),
    }
}

#[test]
fn discovery_deadline_is_reported_at_discovery() {
    let clock = MockClockGuard::new().expect("guard");
    let deadline = clock
        .deadline_after(Duration::from_secs(5))
        .expect("deadline");
    let mut calls = 0u64;
    let mut cancel = || {
        calls += 1;
        if calls >= 3 {
            let _ = clock.expire_deadline();
        }
        false
    };
    match analyze(
        &[1, 2, 3, 4, 5, 6, 7],
        &[1, 2, 3, 4, 5, 6, 7, 8],
        1,
        deadline,
        &mut cancel,
    ) {
        Err(PlanError::BudgetExhausted { stage, timing }) => {
            assert_eq!(stage, AnalysisStage::Discovery);
            assert!(timing.discovery().is_some());
            assert!(timing.search().is_none());
            assert_eq!(
                timing.elapsed(),
                timing.preparation() + timing.discovery().unwrap()
            );
        }
        other => panic!("expected discovery budget error, got {other:?}"),
    }
}

#[test]
fn cancellation_wins_when_timer_expiry_is_also_observed() {
    let clock = MockClockGuard::new().expect("guard");
    let deadline = clock
        .deadline_after(Duration::from_secs(5))
        .expect("deadline");
    let mut calls = 0u64;
    let mut cancel = || {
        calls += 1;
        if calls >= 3 {
            let _ = clock.expire_deadline();
            true
        } else {
            false
        }
    };
    match analyze(&[1, 2, 3, 4], &[1, 2, 3, 4], 1, deadline, &mut cancel) {
        Err(PlanError::Cancelled { stage, timing }) => {
            assert_eq!(stage, AnalysisStage::Discovery);
            assert!(timing.search().is_none());
        }
        other => panic!("expected cancellation, got {other:?}"),
    }
}

#[test]
fn expired_actual_no_match_is_an_error_not_an_empty_plan() {
    let clock = MockClockGuard::new().expect("guard");
    let deadline = clock
        .deadline_after(Duration::from_secs(5))
        .expect("deadline");
    let mut calls = 0u64;
    let mut cancel = || {
        calls += 1;
        if calls >= 3 {
            let _ = clock.expire_deadline();
        }
        false
    };
    match analyze(&[1, 2], &[3, 4], 1, deadline, &mut cancel) {
        Err(PlanError::BudgetExhausted { stage, timing }) => {
            assert_eq!(stage, AnalysisStage::Discovery);
            assert!(timing.discovery().is_some());
            assert!(timing.search().is_none());
        }
        other => panic!("expected discovery error, got {other:?}"),
    }
}

#[test]
fn search_and_finish_cancellation_map_through_public_errors() {
    let context = [1u32, 2, 3, 4, 11, 12, 13, 21, 22];
    let cache = [9u32, 1, 2, 3, 4, 8, 11, 12, 13, 7, 21, 22, 6];
    for (threshold, expected_stage) in [(94, AnalysisStage::Search), (111, AnalysisStage::Finish)] {
        let clock = MockClockGuard::new().expect("guard");
        let deadline = clock
            .deadline_after(Duration::from_secs(10))
            .expect("deadline");
        let mut calls = 0u64;
        let mut cancel = || {
            calls += 1;
            calls >= threshold
        };
        match analyze(&context, &cache, 2, deadline, &mut cancel) {
            Err(PlanError::Cancelled { stage, timing }) => {
                assert_eq!(stage, expected_stage);
                assert!(timing.discovery().is_some());
                assert!(timing.search().is_some());
                assert!(!timing.reached());
            }
            other => panic!("expected cancellation at {expected_stage:?}, got {other:?}"),
        }
    }
}

#[test]
fn feature_enabled_but_unarmed_uses_real_time() {
    let before = Instant::now();
    let deadline = before + Duration::from_secs(5);
    let mut cancel = || false;
    // Known no-work input: no real matching work is needed before the deadline.
    let plan = analyze(&[], &[1], 1, deadline, &mut cancel).expect("ordinary real-clock plan");
    let after = Instant::now();
    let limit = plan.timing().limit();
    // The measured start must be in the same real deadline domain, not a
    // zero-origin mock domain. No fixed tiny-duration threshold or sleep.
    assert!(limit <= Duration::from_secs(5));
    assert!(limit >= deadline.saturating_duration_since(after));
    assert!(plan.ranges().is_empty());
}

#[test]
fn cancellation_during_discovery_wins_before_search() {
    let clock = MockClockGuard::new().expect("guard");
    let deadline = clock
        .deadline_after(Duration::from_secs(5))
        .expect("deadline");
    let mut calls = 0u64;
    let mut cancel = || {
        calls += 1;
        calls >= 4
    };
    match analyze(
        &[1, 2, 3, 4, 5, 6, 7],
        &[1, 2, 3, 4, 5, 6, 7, 8],
        2,
        deadline,
        &mut cancel,
    ) {
        Err(PlanError::Cancelled { stage, timing }) => {
            assert_eq!(stage, AnalysisStage::Discovery);
            assert!(timing.discovery().is_some());
            assert!(timing.search().is_none());
        }
        other => panic!("expected Discovery cancellation, got {other:?}"),
    }
}

#[test]
fn deadline_after_search_entry_reports_search_timing() {
    let clock = MockClockGuard::new().expect("guard");
    let deadline = clock
        .deadline_after(Duration::from_secs(10))
        .expect("deadline");
    let mut calls = 0u64;
    let mut cancel = || {
        calls += 1;
        if calls >= 28 {
            let _ = clock.expire_deadline();
        }
        false
    };
    match analyze(&[1, 2, 3], &[1, 2, 3], 1, deadline, &mut cancel) {
        Err(PlanError::BudgetExhausted { stage, timing }) => {
            assert_eq!(stage, AnalysisStage::Search);
            assert!(timing.discovery().is_some());
            assert!(timing.search().is_some());
            assert_eq!(
                timing.elapsed(),
                timing.preparation() + timing.discovery().unwrap() + timing.search().unwrap()
            );
        }
        other => panic!("expected Search budget after entry, got {other:?}"),
    }
}

#[test]
fn deadline_before_search_entry_reports_search_without_search_timing() {
    let clock = MockClockGuard::new().expect("guard");
    let deadline = clock
        .deadline_after(Duration::from_secs(10))
        .expect("deadline");
    let mut calls = 0u64;
    let mut cancel = || {
        calls += 1;
        if calls >= 26 {
            let _ = clock.expire_deadline();
        }
        false
    };
    match analyze(&[1, 2, 3], &[1, 2, 3], 1, deadline, &mut cancel) {
        Err(PlanError::BudgetExhausted { stage, timing }) => {
            assert_eq!(stage, AnalysisStage::Search);
            assert!(timing.discovery().is_some());
            assert!(timing.search().is_none());
            assert_eq!(
                timing.elapsed(),
                timing.preparation() + timing.discovery().unwrap()
            );
        }
        other => panic!("expected Search entry refusal, got {other:?}"),
    }
}

#[test]
fn nested_guards_are_rejected_and_panic_drop_restores_clean_state() {
    // Rejected nesting must not reset the active clock. The raw read before the
    // fresh guard proves Drop actually reset the mock, rather than new() merely
    // reinitialising it.
    let guard = MockClockGuard::new().expect("first guard");
    guard.advance(Duration::from_secs(3));
    assert!(MockClockGuard::new().is_err());
    assert_eq!(guard.elapsed(), Duration::from_secs(3));
    drop(guard);

    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = MockClockGuard::new().expect("panic guard");
        _guard.advance(Duration::from_secs(2));
        panic!("drop guard during unwind");
    }));
    assert!(caught.is_err());
    assert_eq!(MockClock::time(), Duration::ZERO);
    let _fresh = MockClockGuard::new().expect("clock restored after panic");
}

#[test]
fn clock_overflow_is_loud_and_unwinds_cleanly() {
    let clock = MockClockGuard::new().expect("guard");
    clock.advance(Duration::MAX);
    assert!(clock.deadline_after(Duration::from_secs(1)).is_err());
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut cancel = || false;
        let _ = analyze(
            &[1],
            &[1],
            1,
            Instant::now() + Duration::from_secs(1),
            &mut cancel,
        );
    }));
    assert!(caught.is_err());
    drop(clock);
    assert_eq!(MockClock::time(), Duration::ZERO);
    let _fresh = MockClockGuard::new().expect("clock restored after overflow panic");
}

#[test]
fn thread_local_guards_are_isolated() {
    // Force both guards to coexist, then assert their independent observations
    // only after both threads have passed the rendezvous and joined. A failed
    // guard still reaches the barrier so it cannot strand its peer.
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = [1u64, 2u64]
        .into_iter()
        .map(|seconds| {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || -> Result<(Duration, Duration), &'static str> {
                let clock = match MockClockGuard::new() {
                    Ok(clock) => clock,
                    Err(_) => {
                        barrier.wait();
                        return Err("guard creation failed");
                    }
                };
                clock.advance(Duration::from_secs(seconds));
                let before = clock.elapsed();
                barrier.wait();
                let after = clock.elapsed();
                Ok((before, after))
            })
        })
        .collect();
    let observations: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().expect("thread"))
        .collect();
    for (index, observation) in observations.into_iter().enumerate() {
        let expected = Duration::from_secs(index as u64 + 1);
        assert_eq!(observation, Ok((expected, expected)));
    }
}

#[test]
fn expire_deadline_is_false_after_analysis_returns() {
    let clock = MockClockGuard::new().expect("guard");
    let deadline = clock
        .deadline_after(Duration::from_secs(5))
        .expect("deadline");
    let mut cancel = || false;
    analyze(
        &[1, 2, 3, 4, 5],
        &[9, 1, 2, 3, 4, 5, 9],
        1,
        deadline,
        &mut cancel,
    )
    .expect("matching plan");
    assert!(!clock.expire_deadline());
}

#[test]
fn nested_analysis_restores_the_outer_manual_binding() {
    let clock = MockClockGuard::new().expect("guard");
    let deadline = clock
        .deadline_after(Duration::from_secs(5))
        .expect("deadline");
    let mut outer_calls = 0u64;
    let mut outer_cancel = || {
        outer_calls += 1;
        if outer_calls == 2 {
            let mut inner_cancel = || false;
            analyze(&[1, 2, 3], &[1, 2, 3], 1, deadline, &mut inner_cancel).expect("nested plan");
            assert!(clock.expire_deadline(), "outer binding restored");
        }
        false
    };
    match analyze(&[1, 2, 3], &[1, 2, 3], 1, deadline, &mut outer_cancel) {
        Err(PlanError::BudgetExhausted { stage, timing }) => {
            assert_eq!(stage, AnalysisStage::Preparation);
            assert!(timing.discovery().is_none());
            assert!(timing.search().is_none());
        }
        other => panic!("expected outer expiry, got {other:?}"),
    }
    assert!(
        !clock.expire_deadline(),
        "binding cleared after outer return"
    );
}

#[test]
fn panic_during_analysis_restores_and_clears_the_binding() {
    let clock = MockClockGuard::new().expect("guard");
    let deadline = clock
        .deadline_after(Duration::from_secs(5))
        .expect("deadline");
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut calls = 0u64;
        let mut cancel = || {
            calls += 1;
            if calls == 2 {
                panic!("panic after timer arm");
            }
            false
        };
        let _ = analyze(&[1, 2, 3], &[1, 2, 3], 1, deadline, &mut cancel);
    }));
    assert!(caught.is_err());
    assert!(!clock.expire_deadline());
}

#[test]
fn manual_arm_uses_current_time_for_a_past_deadline() {
    let clock = MockClockGuard::new().expect("guard");
    let deadline = clock
        .deadline_after(Duration::from_secs(5))
        .expect("deadline");
    let mut first = true;
    let mut cancel = || {
        if first {
            first = false;
            clock.advance(Duration::from_secs(6));
        }
        false
    };
    match analyze(&[1, 2, 3], &[1, 2, 3], 1, deadline, &mut cancel) {
        Err(PlanError::BudgetExhausted { stage, timing }) => {
            assert_eq!(stage, AnalysisStage::Preparation);
            assert!(timing.discovery().is_none());
            assert!(timing.search().is_none());
        }
        other => panic!("expected Preparation budget, got {other:?}"),
    }
}
