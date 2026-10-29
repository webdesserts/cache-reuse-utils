//! The public one-shot analysis and its stage orchestration.

use std::time::Instant;

use crate::cache_view::CacheView;
use crate::candidate::MAX_RANGES;
use crate::discovery::{DiscoverySummary, MAX_DISCOVERY_TOKENS, discover};
use crate::error::{AdmissionDeniedReason, ContractError, InputSide, PlanError, PlanErrorKind};
use crate::placement::PlacementPolicy;
use crate::placement_search::{PlacementOutcome, search};
use crate::reuse_plan::ReusePlan;
use crate::timing::{AnalysisStage, Stopper};

/// One cache-reuse analysis request.
///
/// [`Self::analyze`] discovers exact maximal matches between the context and
/// the cache, searches for a placement under the policy, and returns an owned
/// plan or a typed error. It never touches engine cache state.
///
/// `deadline` is one absolute instant shared by every stage. It is checked
/// cooperatively, not enforced preemptively. `cancel` is checked before the
/// deadline at every check, so cancellation always wins.
pub struct CacheReuseAnalysis<'a> {
    /// Target token sequence to reproduce.
    pub context_tokens: &'a [u32],
    /// Borrowed cache source and its caller identity.
    pub cache: CacheView<'a>,
    /// Maximum number of final reused ranges, in `1..=MAX_RANGES`.
    pub k: u32,
    /// Absolute deadline for the whole analysis.
    pub deadline: Instant,
    /// Caller-owned cancellation callback; returning `true` cancels.
    pub cancel: &'a mut dyn FnMut() -> bool,
    /// Placement constraints and selection policy.
    pub placement: PlacementPolicy<'a>,
}

impl CacheReuseAnalysis<'_> {
    /// Validate the request, discover matches and select a plan.
    ///
    /// - A request that cannot match (an empty input, or a minimum longer than
    ///   an input) returns an empty plan immediately, even past the deadline.
    /// - Complete discovery with no qualifying match returns an empty plan.
    /// - A valid plan found before the deadline or a search cap is returned
    ///   with a partial [`crate::PlacementStatus`].
    /// - Without such a plan, the deadline yields
    ///   [`PlanErrorKind::BudgetExhausted`] and a cap yields
    ///   [`PlanErrorKind::PlacementIncomplete`].
    /// - Cancellation always yields [`PlanErrorKind::Cancelled`].
    pub fn analyze(self) -> Result<ReusePlan, PlanError> {
        let mut stopper = Stopper::new(self.deadline, self.cancel);
        run(
            self.context_tokens,
            self.cache,
            self.k,
            &self.placement,
            &mut stopper,
        )
    }
}

fn fail(stopper: &mut Stopper<'_>, kind: impl Into<PlanErrorKind>) -> PlanError {
    PlanError {
        kind: kind.into(),
        stage: stopper.stage(),
        timing: stopper.timing(),
    }
}

fn run(
    context: &[u32],
    cache: CacheView<'_>,
    k: u32,
    policy: &PlacementPolicy<'_>,
    stopper: &mut Stopper<'_>,
) -> Result<ReusePlan, PlanError> {
    if stopper.cancelled() {
        return Err(fail(stopper, PlanErrorKind::Cancelled));
    }
    validate(context, &cache, k, policy).map_err(|kind| fail(stopper, kind))?;

    if cannot_match(context, &cache) {
        return finish(
            context,
            &cache,
            DiscoverySummary::NONE,
            empty(),
            policy,
            stopper,
        );
    }
    stopper.check().map_err(|stop| fail(stopper, stop))?;

    stopper.enter_discovery();
    let window = policy.protected_prefix_tokens as usize
        ..context.len() - policy.fresh_suffix_tokens as usize;
    let discovery =
        discover(context, window, cache, stopper).map_err(|stop| fail(stopper, stop))?;
    if discovery.ranges.is_empty() {
        stopper.check().map_err(|stop| fail(stopper, stop))?;
        return finish(
            context,
            &cache,
            discovery.summary(),
            empty(),
            policy,
            stopper,
        );
    }

    // A refusal here reports stage Search with no search duration.
    stopper.finish_discovery();
    stopper.check().map_err(|stop| fail(stopper, stop))?;
    stopper.start_search();
    let outcome = search(context.len(), &discovery.ranges, k, policy, stopper)
        .map_err(|error| fail(stopper, error))?;
    finish(
        context,
        &cache,
        discovery.summary(),
        outcome,
        policy,
        stopper,
    )
}

fn validate(
    context: &[u32],
    cache: &CacheView<'_>,
    k: u32,
    policy: &PlacementPolicy<'_>,
) -> Result<(), PlanErrorKind> {
    if k == 0 || k > MAX_RANGES {
        return Err(PlanErrorKind::InvalidInput(ContractError::InvalidK {
            k,
            max: MAX_RANGES,
        }));
    }
    if cache.minimum_match_tokens == 0 {
        return Err(PlanErrorKind::InvalidInput(ContractError::InvalidMinimum {
            minimum: 0,
        }));
    }
    policy
        .validate(context.len())
        .map_err(PlanErrorKind::InvalidPlacementPolicy)?;
    for (side, got) in [
        (InputSide::Context, context.len()),
        (InputSide::Cache, cache.tokens.len()),
    ] {
        if got > MAX_DISCOVERY_TOKENS {
            return Err(PlanErrorKind::AdmissionDenied(
                AdmissionDeniedReason::InputTooLarge {
                    side,
                    got,
                    max: MAX_DISCOVERY_TOKENS,
                },
            ));
        }
    }
    Ok(())
}

fn cannot_match(context: &[u32], cache: &CacheView<'_>) -> bool {
    let minimum = cache.minimum_match_tokens as usize;
    minimum > context.len() || minimum > cache.tokens.len()
}

/// The plan when there is nothing to place: search had no work, so it is
/// complete.
fn empty() -> PlacementOutcome {
    PlacementOutcome::empty(0, crate::placement::PlacementStatus::Complete)
}

fn finish(
    context: &[u32],
    cache: &CacheView<'_>,
    discovery: DiscoverySummary,
    outcome: PlacementOutcome,
    policy: &PlacementPolicy<'_>,
    stopper: &mut Stopper<'_>,
) -> Result<ReusePlan, PlanError> {
    stopper.enter_finish();
    let timing = stopper.timing();
    let error = |kind| PlanError {
        kind,
        stage: AnalysisStage::Finish,
        timing,
    };
    if stopper.cancelled() {
        return Err(error(PlanErrorKind::Cancelled));
    }
    ReusePlan::from_outcome(
        context.len(),
        cache.identity,
        discovery,
        outcome,
        policy,
        timing,
    )
    .ok_or_else(|| error(PlanErrorKind::Invariant("inconsistent placement recipe")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache_view::CacheIdentity;
    use std::cell::Cell;
    use std::time::Duration;

    const CONTEXT: [u32; 6] = [1, 2, 3, 4, 5, 6];
    const CACHE: [u32; 8] = [9, 1, 2, 3, 4, 5, 6, 9];

    /// Run with a test clock that sees the current stage.
    fn run_with(
        cancel: &mut dyn FnMut() -> bool,
        clock: impl FnMut(AnalysisStage) -> Instant,
        deadline: Instant,
    ) -> Result<ReusePlan, PlanError> {
        let mut stopper = Stopper::with_clock(deadline, cancel, clock);
        let cache = CacheView {
            tokens: &CACHE,
            identity: CacheIdentity::new(1),
            minimum_match_tokens: 2,
        };
        let policy = PlacementPolicy::without_padding(CONTEXT.len());
        run(&CONTEXT, cache, 1, &policy, &mut stopper)
    }

    /// A clock that reads as past the deadline once `expired(stage)` holds.
    fn expire_when(
        base: Instant,
        deadline: Instant,
        mut expired: impl FnMut(AnalysisStage) -> bool,
    ) -> impl FnMut(AnalysisStage) -> Instant {
        move |stage| if expired(stage) { deadline } else { base }
    }

    #[test]
    fn deadline_in_discovery_stops_before_search() {
        let base = Instant::now();
        let deadline = base + Duration::from_secs(5);
        let mut cancel = || false;
        let clock = expire_when(base, deadline, |stage| stage == AnalysisStage::Discovery);
        let error = run_with(&mut cancel, clock, deadline).expect_err("deadline");
        assert_eq!(error.kind, PlanErrorKind::BudgetExhausted);
        assert_eq!(error.stage, AnalysisStage::Discovery);
        assert!(error.timing.discovery().is_some());
        assert!(error.timing.search().is_none());
    }

    #[test]
    fn deadline_at_search_entry_refuses_search_without_search_timing() {
        let base = Instant::now();
        let deadline = base + Duration::from_secs(5);
        let mut cancel = || false;
        let clock = expire_when(base, deadline, |stage| stage == AnalysisStage::Search);
        let error = run_with(&mut cancel, clock, deadline).expect_err("deadline");
        assert_eq!(error.kind, PlanErrorKind::BudgetExhausted);
        assert_eq!(error.stage, AnalysisStage::Search);
        assert!(error.timing.discovery().is_some());
        assert!(error.timing.search().is_none());
    }

    #[test]
    fn deadline_inside_search_without_a_plan_reports_search_timing() {
        let base = Instant::now();
        let deadline = base + Duration::from_secs(5);
        let mut cancel = || false;
        // Two clock reads close discovery and check search entry.
        let mut search_reads = 0;
        let clock = expire_when(base, deadline, move |stage| {
            if stage == AnalysisStage::Search {
                search_reads += 1;
            }
            search_reads > 2
        });
        let error = run_with(&mut cancel, clock, deadline).expect_err("deadline");
        assert_eq!(error.kind, PlanErrorKind::BudgetExhausted);
        assert_eq!(error.stage, AnalysisStage::Search);
        assert!(error.timing.search().is_some());
    }

    #[test]
    fn cancellation_wins_over_an_expired_deadline() {
        let base = Instant::now();
        let deadline = base + Duration::from_secs(5);
        let in_discovery = Cell::new(false);
        let mut cancel = || in_discovery.get();
        let clock = |stage| {
            if stage == AnalysisStage::Discovery {
                in_discovery.set(true);
                deadline
            } else {
                base
            }
        };
        let error = run_with(&mut cancel, clock, deadline).expect_err("cancelled");
        assert_eq!(error.kind, PlanErrorKind::Cancelled);
        assert_eq!(error.stage, AnalysisStage::Discovery);
    }

    #[test]
    fn cancellation_while_finishing_discards_the_plan() {
        let base = Instant::now();
        let deadline = base + Duration::from_secs(5);
        let finishing = Cell::new(false);
        let mut cancel = || finishing.get();
        let clock = |stage| {
            finishing.set(stage == AnalysisStage::Finish);
            base
        };
        let error = run_with(&mut cancel, clock, deadline).expect_err("cancelled");
        assert_eq!(error.kind, PlanErrorKind::Cancelled);
        assert_eq!(error.stage, AnalysisStage::Finish);
    }

    #[test]
    fn stage_durations_sum_to_elapsed() {
        let base = Instant::now();
        let deadline = base + Duration::from_secs(5);
        let mut cancel = || false;
        let mut reads = 0u64;
        let clock = move |_| {
            reads += 1;
            base + Duration::from_micros(reads)
        };
        let timing = run_with(&mut cancel, clock, deadline)
            .expect("plan")
            .timing();
        assert_eq!(
            timing.elapsed(),
            timing.preparation() + timing.discovery().unwrap() + timing.search().unwrap()
        );
        assert!(timing.search().unwrap() > Duration::ZERO);
    }
}
