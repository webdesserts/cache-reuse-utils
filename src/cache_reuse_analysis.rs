//! Public one-shot cache-reuse analysis and its stage orchestration.

use std::time::Instant;

use crate::cache_view::CacheView;
use crate::candidate::MAX_RANGES;
use crate::candidate_search::CandidateSearch;
use crate::discovery::{DiscoveryStatus, MAX_DISCOVERY_TOKENS, discover};
use crate::error::{AdmissionDeniedReason, ContractError, InputSide, PlanError, geometry_reason};
use crate::reuse_plan::ReusePlan;
use crate::timing::{AnalysisStage, AnalysisTiming, CheckOutcome, DeadlineClock, TimingState};

/// One cache-reuse analysis request.
///
/// This is the README entry point. Callers provide logical context tokens and
/// one borrowed eligible cache source; discovery finds exact qualifying matches
/// and the private selector chooses a legal plan. Native integration still owns
/// projection, cache pinning, physical placement and execution proof.
///
/// `deadline` is one absolute monotonic instant shared by preparation, discovery
/// and search; it is not a per-stage budget. `cancel` is a whole-request stop
/// that takes precedence over the deadline.
pub struct CacheReuseAnalysis<'a> {
    pub context_tokens: &'a [u32],
    pub cache: CacheView<'a>,
    pub k: u32,
    pub deadline: Instant,
    pub cancel: &'a mut dyn FnMut() -> bool,
}

impl<'a> CacheReuseAnalysis<'a> {
    /// Validate input, discover qualifying exact matches, and select a legal
    /// plan under `deadline`.
    ///
    /// Complete discovery with no qualifying match is a deliberate empty plan.
    /// A valid plan found before the deadline is returned even when the deadline
    /// stopped further work. Incomplete discovery with no plan is an
    /// informative error, never a fabricated empty plan. Cancellation wins.
    pub fn analyze(self) -> Result<ReusePlan, PlanError> {
        let CacheReuseAnalysis {
            context_tokens,
            cache,
            k,
            deadline,
            cancel,
        } = self;
        let mut clock = DeadlineClock::start(deadline, cancel);
        analyze_with_clock(context_tokens, cache, k, &mut clock)
    }
}

fn analyze_with_clock(
    context: &[u32],
    cache: CacheView<'_>,
    k: u32,
    clock: &mut DeadlineClock<'_>,
) -> Result<ReusePlan, PlanError> {
    validate_request(context, &cache, k, clock)?;

    // A no-work no-match is a complete result, even past the deadline. It does
    // not fabricate discovery/search stages.
    if cannot_match(context, &cache) {
        let finish = clock.now();
        if clock.cancelled() {
            return Err(PlanError::cancelled(
                AnalysisStage::Preparation,
                AnalysisTiming::preparation_only(clock, finish),
            ));
        }
        return complete_empty_plan(&cache, k, AnalysisTiming::preparation_only(clock, finish));
    }

    // Start the background deadline owner only after validation and the
    // statically known no-work decision. Registration failure fails closed.
    if clock.arm_timer().is_err() {
        let finish = clock.now();
        return Err(PlanError::admission(
            AdmissionDeniedReason::TimerUnavailable,
            AnalysisStage::Preparation,
            AnalysisTiming::preparation_only(clock, finish),
        ));
    }

    // The preparation checkpoint is the last point before discovery.
    match clock.check() {
        CheckOutcome::Cancelled => {
            let finish = clock.now();
            return Err(PlanError::cancelled(
                AnalysisStage::Preparation,
                AnalysisTiming::preparation_only(clock, finish),
            ));
        }
        CheckOutcome::DeadlineReached => {
            let finish = clock.now();
            return Err(PlanError::budget(
                AnalysisStage::Preparation,
                AnalysisTiming::preparation_only(clock, finish),
            ));
        }
        CheckOutcome::Continue => {}
    }

    let mut stages = TimingState::new(clock.now());
    let discovery = match discover(context, cache, clock) {
        Ok(outcome) => outcome,
        Err(error) => {
            let finish = clock.now();
            stages.discovery_finished(finish);
            return Err(PlanError::from_discovery(
                AnalysisStage::Discovery,
                stages.finish(clock, finish),
                error,
            ));
        }
    };
    let discovery_end = clock.now();
    stages.discovery_finished(discovery_end);

    if discovery.ranges.is_empty() {
        // Actual discovery that completes no-match after an observed expiry is
        // not a fabricated empty plan. Only the statically known no-work path
        // above may return empty despite expiry.
        match clock.check() {
            CheckOutcome::Cancelled => {
                return Err(PlanError::cancelled(
                    AnalysisStage::Discovery,
                    stages.finish(clock, discovery_end),
                ));
            }
            CheckOutcome::DeadlineReached => {
                return Err(PlanError::budget(
                    AnalysisStage::Discovery,
                    stages.finish(clock, discovery_end),
                ));
            }
            CheckOutcome::Continue => {}
        }
        return complete_empty_plan(&cache, k, stages.finish(clock, discovery_end));
    }

    // A candidate bag is not an incumbent. Once expiry is observed, refuse
    // search entry; cancellation still wins.
    match clock.check() {
        CheckOutcome::Cancelled => {
            return Err(PlanError::cancelled(
                AnalysisStage::Discovery,
                stages.finish(clock, discovery_end),
            ));
        }
        CheckOutcome::DeadlineReached => {
            return Err(PlanError::budget(
                AnalysisStage::Search,
                stages.finish(clock, discovery_end),
            ));
        }
        CheckOutcome::Continue => {}
    }

    let search_start = clock.now();
    // The search-start checkpoint closes discovery for accounting, including
    // the small boundary interval before search is entered.
    stages.discovery_finished(search_start);
    match clock.check() {
        CheckOutcome::Cancelled => {
            return Err(PlanError::cancelled(
                AnalysisStage::Search,
                stages.finish(clock, search_start),
            ));
        }
        CheckOutcome::DeadlineReached => {
            return Err(PlanError::budget(
                AnalysisStage::Search,
                stages.finish(clock, search_start),
            ));
        }
        CheckOutcome::Continue => {}
    }
    stages.search_started(search_start);
    match CandidateSearch::new(&discovery.ranges, k).run(clock) {
        Err(error) => {
            let finish = clock.now();
            Err(PlanError::from_search(
                AnalysisStage::Search,
                stages.finish(clock, finish),
                error,
            ))
        }
        Ok(ranges) => {
            let finish = clock.now();
            let timing = stages.finish(clock, finish);
            if clock.cancelled() {
                return Err(PlanError::cancelled(AnalysisStage::Finish, timing));
            }
            ReusePlan::from_parts(ranges, k, timing, cache.identity, discovery.status).map_err(
                |error| PlanError::invariant(geometry_reason(error), AnalysisStage::Finish, timing),
            )
        }
    }
}

fn validate_request(
    context: &[u32],
    cache: &CacheView<'_>,
    k: u32,
    clock: &mut DeadlineClock<'_>,
) -> Result<(), PlanError> {
    if clock.cancelled() {
        let finish = clock.now();
        return Err(PlanError::cancelled(
            AnalysisStage::Preparation,
            AnalysisTiming::preparation_only(clock, finish),
        ));
    }
    if k == 0 || k > MAX_RANGES {
        let finish = clock.now();
        return Err(PlanError::invalid_input(
            ContractError::InvalidK { k, max: MAX_RANGES },
            AnalysisStage::Preparation,
            AnalysisTiming::preparation_only(clock, finish),
        ));
    }
    if cache.minimum_match_tokens == 0 {
        let finish = clock.now();
        return Err(PlanError::invalid_input(
            ContractError::InvalidMinimum {
                minimum: cache.minimum_match_tokens,
            },
            AnalysisStage::Preparation,
            AnalysisTiming::preparation_only(clock, finish),
        ));
    }
    let input_side = if context.len() > MAX_DISCOVERY_TOKENS {
        Some((InputSide::Context, context.len()))
    } else if cache.tokens.len() > MAX_DISCOVERY_TOKENS {
        Some((InputSide::Cache, cache.tokens.len()))
    } else {
        None
    };
    if let Some((side, got)) = input_side {
        let finish = clock.now();
        return Err(PlanError::admission(
            AdmissionDeniedReason::InputTooLarge {
                side,
                got,
                max: MAX_DISCOVERY_TOKENS,
            },
            AnalysisStage::Preparation,
            AnalysisTiming::preparation_only(clock, finish),
        ));
    }
    Ok(())
}

fn cannot_match(context: &[u32], cache: &CacheView<'_>) -> bool {
    context.is_empty()
        || cache.tokens.is_empty()
        || cache.minimum_match_tokens as usize > context.len()
        || cache.minimum_match_tokens as usize > cache.tokens.len()
}

fn complete_empty_plan(
    cache: &CacheView<'_>,
    k: u32,
    timing: AnalysisTiming,
) -> Result<ReusePlan, PlanError> {
    ReusePlan::from_parts(
        Vec::new(),
        k,
        timing,
        cache.identity,
        DiscoveryStatus::Complete,
    )
    .map_err(|error| PlanError::invariant(geometry_reason(error), AnalysisStage::Finish, timing))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache_view::CacheIdentity;
    use std::time::Instant;

    #[test]
    fn timer_start_failure_is_preparation_admission_denied() {
        crate::deadline::fail_next_registration();
        let mut cancel = || false;
        let cache = CacheView {
            tokens: &[1, 2, 3],
            identity: CacheIdentity::new(1),
            minimum_match_tokens: 1,
        };
        let mut clock = DeadlineClock::start(
            Instant::now() + std::time::Duration::from_secs(5),
            &mut cancel,
        );
        match analyze_with_clock(&[1, 2, 3], cache, 1, &mut clock) {
            Err(PlanError::AdmissionDenied {
                reason: AdmissionDeniedReason::TimerUnavailable,
                stage: AnalysisStage::Preparation,
                timing,
            }) => {
                assert!(timing.discovery().is_none());
                assert!(timing.search().is_none());
                assert_eq!(timing.preparation(), timing.elapsed());
            }
            other => panic!("expected TimerUnavailable at Preparation, got {other:?}"),
        }
    }
}
