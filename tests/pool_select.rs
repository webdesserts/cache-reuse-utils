//! Choosing between the hot cache, a pool entry and a fresh start.

use cache_reuse_utils::{
    CandidateAction, CandidateInput, CandidateSelection, CandidateSelectionPolicy, score_candidate,
    select_candidate,
};

const POLICY: CandidateSelectionPolicy = CandidateSelectionPolicy {
    minimum_match_tokens: 4,
    alignment: 2,
    restore_penalty_tokens: 3,
};

/// Logical target 0..20.
fn target() -> Vec<u32> {
    (0..20).collect()
}

fn unpadded(physical: &[u32]) -> CandidateInput<'_> {
    CandidateInput {
        physical,
        pad_positions: &[],
        planned_reusable_tokens: 0,
    }
}

fn prefix_of(len: u32) -> Vec<u32> {
    (0..len).chain([99]).collect()
}

#[test]
fn scores_take_the_larger_of_aligned_prefix_and_planned_reuse() {
    let target = target();
    let seven = prefix_of(7);
    // A 7-token common prefix aligns down to 6.
    assert_eq!(score_candidate(&target, unpadded(&seven), POLICY), Some(6));
    let planned = CandidateInput {
        planned_reusable_tokens: 11,
        ..unpadded(&seven)
    };
    assert_eq!(score_candidate(&target, planned, POLICY), Some(11));
    // Planned reuse is capped at the target length.
    let excessive = CandidateInput {
        planned_reusable_tokens: 50,
        ..unpadded(&seven)
    };
    assert_eq!(score_candidate(&target, excessive, POLICY), Some(20));
    // Below the minimum scores 0.
    assert_eq!(
        score_candidate(&target, unpadded(&prefix_of(3)), POLICY),
        Some(0)
    );
}

#[test]
fn scores_skip_pads_and_reject_invalid_input() {
    let target = target();
    // [0, 1, pad, 2, 3, 4] holds logical 0..5 with a pad at 2.
    let physical = [0, 1, 77, 2, 3, 4];
    let padded = CandidateInput {
        physical: &physical,
        pad_positions: &[2],
        planned_reusable_tokens: 0,
    };
    assert_eq!(score_candidate(&target, padded, POLICY), Some(4));
    let invalid = CandidateInput {
        pad_positions: &[6],
        ..padded
    };
    assert_eq!(score_candidate(&target, invalid, POLICY), None);
    let unaligned = CandidateSelectionPolicy {
        alignment: 0,
        ..POLICY
    };
    assert_eq!(score_candidate(&target, padded, unaligned), None);
}

fn select(hot: Option<&[u32]>, pool: &[&[u32]]) -> CandidateSelection {
    let pool: Vec<_> = pool.iter().map(|physical| unpadded(physical)).collect();
    select_candidate(&target(), hot.map(unpadded), &pool, POLICY).unwrap()
}

#[test]
fn a_pool_entry_must_beat_the_hot_cache_by_the_restore_penalty() {
    let (hot, six, ten, twelve) = (prefix_of(8), prefix_of(6), prefix_of(10), prefix_of(12));
    // 10 is not more than 8 + 3: keep hot.
    assert_eq!(
        select(Some(&hot), &[&six, &ten]),
        CandidateSelection {
            action: CandidateAction::KeepHot,
            reusable_tokens: 8
        }
    );
    // 12 is.
    assert_eq!(
        select(Some(&hot), &[&six, &twelve, &ten]),
        CandidateSelection {
            action: CandidateAction::RestorePool { index: 1 },
            reusable_tokens: 12
        }
    );
}

#[test]
fn ties_go_to_the_first_pool_entry_and_nothing_useful_is_fresh() {
    let (eight, twelve, tiny) = (prefix_of(8), prefix_of(12), prefix_of(2));
    assert_eq!(
        select(None, &[&eight, &twelve, &twelve]).action,
        CandidateAction::RestorePool { index: 1 }
    );
    // Without a hot cache, restoring still has to pay for itself.
    assert_eq!(
        select(None, &[&eight]).action,
        CandidateAction::RestorePool { index: 0 }
    );
    let fresh = CandidateSelection {
        action: CandidateAction::Fresh,
        reusable_tokens: 0,
    };
    assert_eq!(select(None, &[]), fresh);
    assert_eq!(select(Some(&tiny), &[&tiny]), fresh);
    let expensive = CandidateSelectionPolicy {
        restore_penalty_tokens: u32::MAX,
        ..POLICY
    };
    let pool = [unpadded(&twelve)];
    assert_eq!(
        select_candidate(&target(), None, &pool, expensive),
        Some(fresh)
    );
}

#[test]
fn any_invalid_candidate_rejects_the_selection() {
    let good = prefix_of(8);
    let bad = CandidateInput {
        physical: &good,
        pad_positions: &[3, 3],
        planned_reusable_tokens: 0,
    };
    assert_eq!(
        select_candidate(&target(), Some(bad), &[unpadded(&good)], POLICY),
        None
    );
    assert_eq!(
        select_candidate(&target(), None, &[unpadded(&good), bad], POLICY),
        None
    );
}
