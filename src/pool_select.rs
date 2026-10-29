//! Choose which cached sequence to reuse for a target: the hot one already
//! loaded, one restored from a pool, or none.

use crate::coordinates::common_logical_prefix;

/// One cached sequence to score against a logical target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CandidateInput<'a> {
    /// Physical tokens of the cached sequence, pads included.
    pub physical: &'a [u32],
    /// Pad positions in `physical`; see [`crate::validate_pad_positions`].
    pub pad_positions: &'a [u32],
    /// Target tokens a placement plan reuses from this candidate, such as
    /// [`crate::ReusePlan::covered_tokens`], or 0 when none was planned.
    /// Planning is the caller's: it owns the per-candidate time budget.
    pub planned_reusable_tokens: u32,
}

/// How candidates are scored and compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CandidateSelectionPolicy {
    /// A candidate reusing fewer tokens scores 0.
    pub minimum_match_tokens: u32,
    /// Common-prefix reuse is rounded down to a multiple of this; nonzero.
    pub alignment: u32,
    /// Reusable tokens a pool candidate must exceed the hot candidate by to
    /// pay for restoring it.
    pub restore_penalty_tokens: u32,
}

/// What to do with the caches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateAction {
    /// Keep the hot candidate.
    KeepHot,
    /// Restore the pool candidate at `index`.
    RestorePool { index: u32 },
    /// No candidate is worth reusing.
    Fresh,
}

/// Result of [`select_candidate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CandidateSelection {
    pub action: CandidateAction,
    /// Score of the chosen candidate; 0 for [`CandidateAction::Fresh`].
    pub reusable_tokens: u32,
}

/// Target tokens `candidate` can supply: the larger of its aligned common
/// logical prefix with `logical_target` and its planned reuse, capped at the
/// target length, or 0 below `policy.minimum_match_tokens`.
///
/// Returns `None` for invalid pad positions or a zero alignment.
///
/// ```
/// # use cache_reuse_utils::{CandidateInput, CandidateSelectionPolicy, score_candidate};
/// let policy = CandidateSelectionPolicy { minimum_match_tokens: 2, alignment: 2, restore_penalty_tokens: 0 };
/// let candidate = CandidateInput { physical: &[1, 2, 3, 9], pad_positions: &[], planned_reusable_tokens: 0 };
/// // Common prefix 3, aligned down to 2.
/// assert_eq!(score_candidate(&[1, 2, 3, 4], candidate, policy), Some(2));
/// ```
pub fn score_candidate(
    logical_target: &[u32],
    candidate: CandidateInput<'_>,
    policy: CandidateSelectionPolicy,
) -> Option<u32> {
    if policy.alignment == 0 {
        return None;
    }
    let prefix =
        common_logical_prefix(logical_target, candidate.physical, candidate.pad_positions)?;
    let aligned_prefix = prefix.common_logical_tokens / policy.alignment * policy.alignment;
    let target_len = u32::try_from(logical_target.len()).unwrap_or(u32::MAX);
    let reusable = aligned_prefix
        .max(candidate.planned_reusable_tokens)
        .min(target_len);
    Some(if reusable < policy.minimum_match_tokens {
        0
    } else {
        reusable
    })
}

/// Choose between keeping `hot`, restoring one of `pool`, or starting fresh.
///
/// A pool candidate wins only when its score exceeds the hot score (0
/// without a hot candidate) by more than `restore_penalty_tokens`; ties go
/// to the lowest index. Otherwise a positive hot score keeps the hot
/// candidate, and anything else is fresh.
///
/// Returns `None` if any candidate is invalid (see [`score_candidate`]) or
/// the pool has more than `u32::MAX` entries.
///
/// ```
/// # use cache_reuse_utils::*;
/// let policy = CandidateSelectionPolicy { minimum_match_tokens: 1, alignment: 1, restore_penalty_tokens: 1 };
/// let candidate = |physical| CandidateInput { physical, pad_positions: &[], planned_reusable_tokens: 0 };
/// let hot = candidate(&[1, 9]);
/// let pool = [candidate(&[1, 2, 3])];
/// let selection = select_candidate(&[1, 2, 3], Some(hot), &pool, policy).unwrap();
/// assert_eq!(selection.action, CandidateAction::RestorePool { index: 0 });
/// assert_eq!(selection.reusable_tokens, 3);
/// ```
pub fn select_candidate(
    logical_target: &[u32],
    hot: Option<CandidateInput<'_>>,
    pool: &[CandidateInput<'_>],
    policy: CandidateSelectionPolicy,
) -> Option<CandidateSelection> {
    u32::try_from(pool.len()).ok()?;
    let hot_score = match hot {
        Some(candidate) => score_candidate(logical_target, candidate, policy)?,
        None => 0,
    };
    let mut best: Option<(u32, u32)> = None;
    for (index, &candidate) in pool.iter().enumerate() {
        let score = score_candidate(logical_target, candidate, policy)?;
        if best.is_none_or(|(_, best_score)| score > best_score) {
            best = Some((index as u32, score));
        }
    }
    let selection = match best {
        Some((index, score)) if score > hot_score.saturating_add(policy.restore_penalty_tokens) => {
            CandidateSelection {
                action: CandidateAction::RestorePool { index },
                reusable_tokens: score,
            }
        }
        _ if hot_score > 0 => CandidateSelection {
            action: CandidateAction::KeepHot,
            reusable_tokens: hot_score,
        },
        _ => CandidateSelection {
            action: CandidateAction::Fresh,
            reusable_tokens: 0,
        },
    };
    Some(selection)
}
