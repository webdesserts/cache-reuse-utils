//! Placement policy, recipes, metrics and configuration validation.
//!
//! This is the stateless placement contract: source tokens are actual cached
//! content, target tokens are content to preserve, and only insertions made by
//! this analysis count as padding. There is no source layout or carried-pad
//! provenance.

use crate::cache_view::CacheIdentity;

/// Relative completion-loss allowance.
///
/// The value is stored in basis points: one basis point is 0.01%, so 1,000
/// basis points is 10%. Use [`LossAllowance::from_percent`] for the common
/// whole-percent form and [`LossAllowance::from_basis_points`] when a caller
/// needs finer precision. Values above 10,000 basis points are rejected by
/// policy validation during analysis, not silently clamped.
///
/// ```
/// # use cache_reuse_utils::LossAllowance;
/// let ten_percent = LossAllowance::from_percent(10);
/// assert_eq!(ten_percent, LossAllowance::from_basis_points(1_000));
/// assert_eq!(ten_percent.basis_points(), 1_000);
/// assert_eq!(LossAllowance::default(), ten_percent);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LossAllowance {
    basis_points: u16,
}

impl LossAllowance {
    /// Construct from a whole percentage, such as `from_percent(10)`.
    ///
    /// The value is not silently clamped. Percentages above 100 produce a
    /// basis-point value that policy validation rejects with
    /// [`PlacementConfigError::InvalidLossAllowance`].
    pub const fn from_percent(percent: u8) -> Self {
        Self {
            basis_points: percent as u16 * 100,
        }
    }

    /// Construct from an exact basis-point value for fractional precision.
    ///
    /// One basis point is 0.01%; `from_basis_points(1)` is 0.01% and
    /// `from_basis_points(1_000)` is 10%. Values above 10,000 are rejected by
    /// policy validation rather than clamped.
    pub const fn from_basis_points(basis_points: u16) -> Self {
        Self { basis_points }
    }

    /// Return the stored basis points. This is the loss unit used by integer
    /// admission arithmetic, not a percentage.
    pub const fn basis_points(self) -> u16 {
        self.basis_points
    }

    /// Return the allowance as a percentage for display, not for admission
    /// arithmetic.
    pub const fn percent(self) -> f64 {
        self.basis_points as f64 / 100.0
    }

    /// Smallest coverage within this allowance of `best`:
    /// `best - floor(best * basis_points / 10_000)`.
    pub(crate) fn floor(self, best: u64) -> u64 {
        best - best.saturating_mul(self.basis_points as u64) / 10_000
    }
}

impl Default for LossAllowance {
    fn default() -> Self {
        Self::from_percent(10)
    }
}

/// Default cap on placement variants derived from discovered matches.
///
/// Reaching it returns the best plan found with
/// [`PlacementStatus::VariantLimitReached`], or
/// [`crate::PlanErrorKind::PlacementIncomplete`] when there is none.
pub const MAX_PLACEMENT_VARIANTS: usize = 4096;

/// Largest accepted `max_new_padding`. A larger value is an invalid policy.
pub const MAX_NEW_PADDING: u32 = 4096;

/// Default `pad_penalty_tokens` for [`PlacementPolicy::with_padding`].
pub const DEFAULT_PAD_PENALTY_TOKENS: u32 = 16;

/// Original-target boundaries where this call may insert padding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllowedInsertions<'a> {
    /// Any boundary `0..=context_len`.
    All,
    /// Only these boundaries, sorted and unique. Empty permits none.
    Boundaries(&'a [u32]),
}

impl AllowedInsertions<'_> {
    /// The latest permitted boundary in `low..=high`.
    pub(crate) fn latest_in(&self, low: u32, high: u32) -> Option<u32> {
        match *self {
            Self::All => Some(high),
            Self::Boundaries(boundaries) => {
                let end = boundaries.partition_point(|&value| value <= high);
                let boundary = *boundaries.get(end.checked_sub(1)?)?;
                (boundary >= low).then_some(boundary)
            }
        }
    }
}

/// Caller-declared placement constraints. All tokens are actual content; no
/// token value is interpreted as padding.
///
/// Use [`Self::without_padding`] for an intentional no-insertion analysis, or
/// [`Self::with_padding`] when a caller supplies a pad token, allowed insertion
/// boundaries and prepared capacity. Fields remain public for adapter-specific
/// refinements such as native alignment.
#[derive(Debug, Clone, Copy)]
pub struct PlacementPolicy<'a> {
    /// Relative coverage loss accepted in exchange for less padding.
    pub loss_allowance: LossAllowance,
    /// Leading original-target tokens that must remain fresh.
    pub protected_prefix_tokens: u32,
    /// Trailing original-target tokens that must remain fresh.
    pub fresh_suffix_tokens: u32,
    /// Boundaries where this call may insert padding.
    pub allowed_insertions: AllowedInsertions<'a>,
    /// Model-visible token for this call's insertions. `None` forbids padding.
    pub pad_token: Option<u32>,
    /// Covered tokens one inserted pad must buy back. Selection maximizes
    /// `covered - padding * pad_penalty_tokens` among eligible plans.
    pub pad_penalty_tokens: u32,
    /// Required alignment of source-range starts.
    pub source_start_alignment: u32,
    /// Required alignment of prepared-target range starts.
    pub prepared_target_start_alignment: u32,
    /// Required alignment of reused token counts.
    pub reuse_count_alignment: u32,
    /// Smallest prepared-target position at which a reused span may start.
    pub minimum_prepared_reuse_start: u32,
    /// Smallest actual source count for a selected range.
    pub minimum_physical_reuse_tokens: u32,
    /// Maximum total original target length plus inserted padding.
    pub max_prepared_tokens: usize,
    /// Maximum current-call padding. With [`Self::with_padding`] this is derived
    /// from spare prepared capacity and bounded by [`MAX_NEW_PADDING`].
    pub max_new_padding: u32,
    /// Extra left trims to try per match. Trims needed to clear a previous
    /// range or meet alignment are always applied and need no allowance.
    pub max_trim_left: u32,
    /// Extra right trims to try per match.
    pub max_trim_right: u32,
    /// Cap on derived variants, in `1..=MAX_PLACEMENT_VARIANTS`.
    pub max_variants: usize,
}

impl<'a> PlacementPolicy<'a> {
    /// Construct a no-padding policy for `context_len`.
    ///
    /// It forbids insertions and caps prepared tokens at the original target
    /// length. Alignments are one and no extra trims are tried.
    pub fn without_padding(context_len: usize) -> Self {
        Self {
            loss_allowance: LossAllowance::default(),
            protected_prefix_tokens: 0,
            fresh_suffix_tokens: 0,
            allowed_insertions: AllowedInsertions::Boundaries(&[]),
            pad_token: None,
            pad_penalty_tokens: 0,
            source_start_alignment: 1,
            prepared_target_start_alignment: 1,
            reuse_count_alignment: 1,
            minimum_prepared_reuse_start: 0,
            minimum_physical_reuse_tokens: 1,
            max_prepared_tokens: context_len,
            max_new_padding: 0,
            max_trim_left: 0,
            max_trim_right: 0,
            max_variants: MAX_PLACEMENT_VARIANTS,
        }
    }

    /// Construct a padding-capable policy with caller-owned inputs.
    ///
    /// `prepared_capacity` is the total original-target-plus-padding capacity;
    /// `max_new_padding` is the spare capacity, capped by [`MAX_NEW_PADDING`].
    /// Each pad costs [`DEFAULT_PAD_PENALTY_TOKENS`] covered tokens in
    /// selection. Alignments start at 1; set the fields a model requires.
    /// Invalid capacity or boundaries are rejected during analysis.
    ///
    /// ```
    /// # use cache_reuse_utils::{AllowedInsertions, PlacementPolicy};
    /// let policy = PlacementPolicy::with_padding(10, 99, AllowedInsertions::All, 11);
    /// assert_eq!(policy.pad_token, Some(99));
    /// assert_eq!(policy.max_new_padding, 1);
    /// assert_eq!(policy.pad_penalty_tokens, 16);
    /// ```
    pub fn with_padding(
        context_len: usize,
        pad_token: u32,
        allowed_insertions: AllowedInsertions<'a>,
        prepared_capacity: usize,
    ) -> Self {
        let max_new_padding = prepared_capacity
            .saturating_sub(context_len)
            .min(MAX_NEW_PADDING as usize) as u32;
        let mut policy = Self::without_padding(context_len);
        policy.pad_token = Some(pad_token);
        policy.allowed_insertions = allowed_insertions;
        policy.pad_penalty_tokens = DEFAULT_PAD_PENALTY_TOKENS;
        policy.max_prepared_tokens = prepared_capacity;
        policy.max_new_padding = max_new_padding;
        policy
    }

    pub(crate) fn validate(&self, context_len: usize) -> Result<(), PlacementConfigError> {
        if self.loss_allowance.basis_points() > 10_000 {
            return Err(PlacementConfigError::InvalidLossAllowance {
                basis_points: self.loss_allowance.basis_points(),
            });
        }
        if self.source_start_alignment == 0
            || self.prepared_target_start_alignment == 0
            || self.reuse_count_alignment == 0
        {
            return Err(PlacementConfigError::ZeroAlignment);
        }
        if self.protected_prefix_tokens as usize > context_len
            || self.fresh_suffix_tokens as usize
                > context_len.saturating_sub(self.protected_prefix_tokens as usize)
        {
            return Err(PlacementConfigError::PrefixSuffixOutOfRange);
        }
        if self.minimum_physical_reuse_tokens == 0 {
            return Err(PlacementConfigError::ZeroPhysicalMinimum);
        }
        if self.max_prepared_tokens < context_len || self.max_prepared_tokens > u32::MAX as usize {
            return Err(PlacementConfigError::InvalidCapacity {
                got: self.max_prepared_tokens,
                min: context_len,
            });
        }
        if self.max_new_padding > MAX_NEW_PADDING {
            return Err(PlacementConfigError::PaddingLimitTooLarge {
                got: self.max_new_padding,
                max: MAX_NEW_PADDING,
            });
        }
        if self.max_variants == 0 || self.max_variants > MAX_PLACEMENT_VARIANTS {
            return Err(PlacementConfigError::InvalidVariantLimit {
                got: self.max_variants,
                max: MAX_PLACEMENT_VARIANTS,
            });
        }
        if self.max_new_padding > 0 && self.pad_token.is_none() {
            return Err(PlacementConfigError::MissingPadToken);
        }
        let boundaries = match self.allowed_insertions {
            AllowedInsertions::All => &[][..],
            AllowedInsertions::Boundaries(boundaries) => boundaries,
        };
        let mut previous = None;
        for &boundary in boundaries {
            if boundary as usize > context_len {
                return Err(PlacementConfigError::InsertionBoundaryOutOfRange {
                    boundary,
                    context_len,
                });
            }
            if previous.is_some_and(|value| value >= boundary) {
                return Err(PlacementConfigError::UnsortedInsertions { boundary });
            }
            previous = Some(boundary);
        }
        Ok(())
    }
}

/// Invalid placement policy supplied by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlacementConfigError {
    InvalidLossAllowance { basis_points: u16 },
    ZeroAlignment,
    PrefixSuffixOutOfRange,
    ZeroPhysicalMinimum,
    InvalidCapacity { got: usize, min: usize },
    PaddingLimitTooLarge { got: u32, max: u32 },
    InvalidVariantLimit { got: usize, max: usize },
    MissingPadToken,
    InsertionBoundaryOutOfRange { boundary: u32, context_len: usize },
    UnsortedInsertions { boundary: u32 },
}

/// One final reused range in original/source and prepared coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlacedRange {
    /// First reused token in actual source coordinates.
    pub source_start: u32,
    /// First replaced token in original target coordinates.
    pub destination_start: u32,
    /// Number of reused tokens.
    pub token_count: u32,
    /// First prepared-target token after previous insertions.
    pub prepared_start: u32,
}

impl PlacedRange {
    /// Exclusive end in actual source coordinates.
    pub fn source_end(self) -> u32 {
        self.source_start + self.token_count
    }

    /// Exclusive end in original target coordinates.
    pub fn destination_end(self) -> u32 {
        self.destination_start + self.token_count
    }

    /// Exclusive end in prepared target coordinates.
    pub fn prepared_end(self) -> u32 {
        self.prepared_start + self.token_count
    }
}

/// Current-call insertion before an original target boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Insertion {
    /// Original-target boundary before which the tokens are inserted.
    pub before_target_index: u32,
    /// Model-visible token value inserted.
    pub token: u32,
    /// Number of tokens inserted at this boundary.
    pub count: u32,
}

/// Complete placement recipe. There is no source-provenance record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementRecipe {
    /// Caller identity from the analyzed cache view.
    pub cache_identity: CacheIdentity,
    /// Length of the original target before current-call insertions.
    pub original_tokens: u32,
    /// Length of the prepared target including current-call insertions.
    pub prepared_tokens: u32,
    /// Final reused ranges in source, original-target and prepared coordinates.
    pub ranges: Vec<PlacedRange>,
    /// Current-call insertions, ordered by target boundary.
    pub insertions: Vec<Insertion>,
}

impl PlacementRecipe {
    /// Map an original target boundary to its prepared position.
    ///
    /// Returns `None` for an index beyond the original target, an invalid
    /// recipe (see [`Self::validate`]), or a mapping position that cannot be
    /// represented as `u32`.
    pub fn prepared_position(&self, original_index: u32) -> Option<PlacementPosition> {
        self.validate().ok()?;
        if original_index > self.original_tokens {
            return None;
        }
        let inserted = self
            .insertions
            .iter()
            .filter(|insertion| insertion.before_target_index < original_index)
            .try_fold(0u64, |total, insertion| {
                total.checked_add(insertion.count as u64)
            })?;
        let after = self
            .insertions
            .iter()
            .filter(|insertion| insertion.before_target_index <= original_index)
            .try_fold(0u64, |total, insertion| {
                total.checked_add(insertion.count as u64)
            })?;
        Some(PlacementPosition {
            before_insertions: u32::try_from(original_index as u64 + inserted).ok()?,
            after_insertions: u32::try_from(original_index as u64 + after).ok()?,
        })
    }

    /// Check structural consistency of prepared ranges/insertions. This is not
    /// token-identity or native-execution proof.
    pub fn validate(&self) -> Result<(), PlacementMaterializeError> {
        if self.prepared_tokens < self.original_tokens {
            return Err(PlacementMaterializeError::RecipeShape);
        }
        let inserted: u64 = self
            .insertions
            .iter()
            .map(|insertion| insertion.count as u64)
            .sum();
        if self.prepared_tokens as u64 != self.original_tokens as u64 + inserted {
            return Err(PlacementMaterializeError::RecipeShape);
        }
        let mut previous_insertion = None;
        for insertion in &self.insertions {
            if insertion.count == 0
                || insertion.before_target_index > self.original_tokens
                || previous_insertion.is_some_and(|value| value >= insertion.before_target_index)
            {
                return Err(PlacementMaterializeError::RecipeShape);
            }
            previous_insertion = Some(insertion.before_target_index);
        }
        let mut previous_end: Option<(u32, u32, u32)> = None;
        for range in &self.ranges {
            let source_end = range
                .source_start
                .checked_add(range.token_count)
                .ok_or(PlacementMaterializeError::RecipeShape)?;
            let destination_end = range
                .destination_start
                .checked_add(range.token_count)
                .ok_or(PlacementMaterializeError::RecipeShape)?;
            let prepared_end = range
                .prepared_start
                .checked_add(range.token_count)
                .ok_or(PlacementMaterializeError::RecipeShape)?;
            if range.token_count == 0
                || destination_end > self.original_tokens
                || prepared_end > self.prepared_tokens
            {
                return Err(PlacementMaterializeError::RecipeShape);
            }
            if let Some((previous_source_end, previous_destination_end, previous_prepared_end)) =
                previous_end
                && (range.source_start < previous_source_end
                    || range.destination_start < previous_destination_end
                    || range.prepared_start < previous_prepared_end)
            {
                return Err(PlacementMaterializeError::RecipeShape);
            }
            let insertions_before = self
                .insertions
                .iter()
                .filter(|insertion| insertion.before_target_index <= range.destination_start)
                .try_fold(0u64, |total, insertion| {
                    total.checked_add(insertion.count as u64)
                })
                .ok_or(PlacementMaterializeError::RecipeShape)?;
            let expected_prepared = (range.destination_start as u64)
                .checked_add(insertions_before)
                .ok_or(PlacementMaterializeError::RecipeShape)?;
            if expected_prepared != range.prepared_start as u64 {
                return Err(PlacementMaterializeError::RecipeShape);
            }
            for insertion in &self.insertions {
                if insertion.before_target_index > range.destination_start
                    && insertion.before_target_index < destination_end
                {
                    return Err(PlacementMaterializeError::RecipeShape);
                }
            }
            previous_end = Some((source_end, destination_end, prepared_end));
        }
        Ok(())
    }

    /// Build the prepared target token buffer.
    ///
    /// Walks the original `context`, inserts this recipe's pads at their
    /// boundaries and copies reused spans from `source`. Every reused token
    /// must equal the context token it replaces. This builds tokens only; it
    /// does not touch native cache state.
    ///
    /// ```
    /// # use cache_reuse_utils::{CacheIdentity, Insertion, PlacedRange, PlacementRecipe};
    /// let recipe = PlacementRecipe {
    ///     cache_identity: CacheIdentity::new(1),
    ///     original_tokens: 4,
    ///     prepared_tokens: 5,
    ///     ranges: vec![PlacedRange {
    ///         source_start: 0,
    ///         destination_start: 1,
    ///         token_count: 2,
    ///         prepared_start: 2,
    ///     }],
    ///     insertions: vec![Insertion {
    ///         before_target_index: 1,
    ///         token: 9,
    ///         count: 1,
    ///     }],
    /// };
    /// let prepared = recipe.materialize(&[0, 1, 2, 3], &[1, 2]).unwrap();
    /// assert_eq!(prepared, vec![0, 9, 1, 2, 3]);
    /// ```
    pub fn materialize(
        &self,
        context: &[u32],
        source: &[u32],
    ) -> Result<Vec<u32>, PlacementMaterializeError> {
        self.validate()?;
        if context.len() != self.original_tokens as usize {
            return Err(PlacementMaterializeError::InputShape);
        }
        let mut out = Vec::new();
        out.try_reserve_exact(self.prepared_tokens as usize)
            .map_err(|_| PlacementMaterializeError::Allocation)?;
        let mut insert_index = 0usize;
        let mut range_index = 0usize;
        let mut index = 0usize;
        while index < context.len() {
            while insert_index < self.insertions.len()
                && self.insertions[insert_index].before_target_index as usize == index
            {
                let insertion = self.insertions[insert_index];
                let remaining = (self.prepared_tokens as usize)
                    .checked_sub(out.len())
                    .ok_or(PlacementMaterializeError::RecipeShape)?;
                if insertion.count as usize > remaining {
                    return Err(PlacementMaterializeError::RecipeShape);
                }
                for _ in 0..insertion.count {
                    out.push(insertion.token);
                }
                insert_index += 1;
            }
            if let Some(range) = self.ranges.get(range_index).copied() {
                if (range.destination_start as usize) < index {
                    return Err(PlacementMaterializeError::RecipeShape);
                }
                if range.destination_start as usize == index {
                    if out.len() != range.prepared_start as usize {
                        return Err(PlacementMaterializeError::RecipeShape);
                    }
                    for offset in 0..range.token_count as usize {
                        let target_index = index + offset;
                        let value = *source
                            .get(range.source_start as usize + offset)
                            .ok_or(PlacementMaterializeError::SourceRange)?;
                        if target_index >= context.len() || value != context[target_index] {
                            return Err(PlacementMaterializeError::ContentMismatch {
                                index: target_index,
                            });
                        }
                        out.push(value);
                    }
                    index += range.token_count as usize;
                    range_index += 1;
                    continue;
                }
            }
            out.push(context[index]);
            index += 1;
        }
        while insert_index < self.insertions.len()
            && self.insertions[insert_index].before_target_index as usize == context.len()
        {
            let insertion = self.insertions[insert_index];
            let remaining = (self.prepared_tokens as usize)
                .checked_sub(out.len())
                .ok_or(PlacementMaterializeError::RecipeShape)?;
            if insertion.count as usize > remaining {
                return Err(PlacementMaterializeError::RecipeShape);
            }
            for _ in 0..insertion.count {
                out.push(insertion.token);
            }
            insert_index += 1;
        }
        if out.len() != self.prepared_tokens as usize
            || range_index != self.ranges.len()
            || insert_index != self.insertions.len()
        {
            return Err(PlacementMaterializeError::RecipeShape);
        }
        Ok(out)
    }
}

/// Prepared positions for an original target boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlacementPosition {
    /// Position before any insertions recorded at this boundary.
    pub before_insertions: u32,
    /// Position after insertions recorded at this boundary.
    pub after_insertions: u32,
}

/// Why a recipe could not be validated or materialized.
///
/// `RecipeShape` means the recipe is structurally inconsistent; the other
/// variants describe the inputs passed to [`PlacementRecipe::materialize`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacementMaterializeError {
    InputShape,
    SourceRange,
    ContentMismatch { index: usize },
    RecipeShape,
    Allocation,
}

/// Plan-dependent placement accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlacementMetrics {
    /// Useful original-target tokens covered by this plan.
    pub useful_tokens: u64,
    /// Current-call tokens inserted into the prepared target.
    pub inserted_padding: u32,
    /// Prepared target length including insertions.
    pub prepared_tokens: u32,
    /// Original-target tokens not covered by reuse.
    pub fresh_tokens: u32,
    /// Best completed useful coverage found during the search.
    pub best_useful: u64,
    /// Minimum useful coverage eligible after applying the loss allowance.
    pub admissible_min: u64,
    /// Allowance used to derive `admissible_min`.
    pub loss_allowance: LossAllowance,
}

/// How completely search explored the placements for a returned plan.
///
/// A partial status still carries the best valid plan found. Without one,
/// analysis returns an error instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacementStatus {
    /// Every derived variant was searched. Also reported, with an empty plan,
    /// when discovery found no match to place.
    Complete,
    /// Matches were found but none could be placed under the policy; the
    /// plan is deliberately empty.
    NoFeasibleReuse,
    /// Search stopped at the deadline.
    DeadlinePartial,
    /// The variant cap dropped some matches or trims.
    VariantLimitReached,
    /// The plan-state cap stopped search.
    StateLimitReached,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recipe_materializes_insertions_and_exact_copied_content() {
        let recipe = PlacementRecipe {
            cache_identity: CacheIdentity::new(1),
            original_tokens: 4,
            prepared_tokens: 5,
            ranges: vec![PlacedRange {
                source_start: 0,
                destination_start: 1,
                token_count: 2,
                prepared_start: 2,
            }],
            insertions: vec![Insertion {
                before_target_index: 1,
                token: 9,
                count: 1,
            }],
        };
        let context = [0, 1, 2, 3];
        let source = [1, 2];
        assert_eq!(
            recipe.materialize(&context, &source).expect("materialize"),
            vec![0, 9, 1, 2, 3]
        );
        assert_eq!(
            recipe.prepared_position(1),
            Some(PlacementPosition {
                before_insertions: 1,
                after_insertions: 2
            })
        );
        assert_eq!(
            recipe.materialize(&[0, 1, 2, 3], &[1, 7]),
            Err(PlacementMaterializeError::ContentMismatch { index: 2 })
        );
    }

    #[test]
    fn hand_built_malformed_recipe_helpers_fail_before_overflow_or_huge_work() {
        let recipe = PlacementRecipe {
            cache_identity: CacheIdentity::new(1),
            original_tokens: 4,
            prepared_tokens: 5,
            ranges: vec![],
            insertions: vec![
                Insertion {
                    before_target_index: 0,
                    token: 1,
                    count: u32::MAX,
                },
                Insertion {
                    before_target_index: 1,
                    token: 2,
                    count: u32::MAX,
                },
            ],
        };
        assert_eq!(
            recipe.validate(),
            Err(PlacementMaterializeError::RecipeShape)
        );
        assert_eq!(recipe.prepared_position(4), None);
        assert_eq!(
            recipe.materialize(&[1, 2, 3, 4], &[]),
            Err(PlacementMaterializeError::RecipeShape)
        );
    }

    #[test]
    fn policy_validation_rejects_inconsistent_limits() {
        let mut policy = PlacementPolicy::without_padding(4);
        assert!(policy.validate(4).is_ok());
        policy.loss_allowance = LossAllowance::from_basis_points(10_001);
        assert!(matches!(
            policy.validate(4),
            Err(PlacementConfigError::InvalidLossAllowance { .. })
        ));
        let mut policy = PlacementPolicy::without_padding(4);
        policy.max_new_padding = 1;
        assert_eq!(
            policy.validate(4),
            Err(PlacementConfigError::MissingPadToken)
        );
        let mut policy = PlacementPolicy::without_padding(4);
        policy.allowed_insertions = AllowedInsertions::Boundaries(&[2, 1]);
        assert!(matches!(
            policy.validate(4),
            Err(PlacementConfigError::UnsortedInsertions { .. })
        ));
        let mut policy = PlacementPolicy::without_padding(4);
        policy.max_prepared_tokens = 3;
        assert!(matches!(
            policy.validate(4),
            Err(PlacementConfigError::InvalidCapacity { .. })
        ));
        let mut policy = PlacementPolicy::without_padding(4);
        policy.max_new_padding = MAX_NEW_PADDING + 1;
        policy.pad_token = Some(1);
        assert!(matches!(
            policy.validate(4),
            Err(PlacementConfigError::PaddingLimitTooLarge { .. })
        ));
        let mut policy = PlacementPolicy::without_padding(4);
        policy.max_variants = MAX_PLACEMENT_VARIANTS + 1;
        assert!(matches!(
            policy.validate(4),
            Err(PlacementConfigError::InvalidVariantLimit { .. })
        ));
        let mut policy = PlacementPolicy::without_padding(4);
        policy.protected_prefix_tokens = 3;
        policy.fresh_suffix_tokens = 2;
        assert_eq!(
            policy.validate(4),
            Err(PlacementConfigError::PrefixSuffixOutOfRange)
        );
        let mut policy = PlacementPolicy::without_padding(4);
        policy.allowed_insertions = AllowedInsertions::Boundaries(&[5]);
        assert!(matches!(
            policy.validate(4),
            Err(PlacementConfigError::InsertionBoundaryOutOfRange { .. })
        ));
        let mut policy = PlacementPolicy::without_padding(4);
        policy.source_start_alignment = 0;
        assert_eq!(policy.validate(4), Err(PlacementConfigError::ZeroAlignment));
    }

    #[test]
    fn loss_allowance_floor_examples_are_integer_and_documented() {
        let floor = |best: u64, bp: u16| LossAllowance::from_basis_points(bp).floor(best);
        assert_eq!(floor(980, 1_000), 882);
        assert_eq!(floor(200, 1_000), 180);
        assert_eq!(floor(199, 1_000), 180);
        assert_eq!(floor(1, 1_000), 1);
        assert_eq!(floor(980, 10_000), 0);
    }
}
