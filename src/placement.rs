//! Placement policy, recipes, metrics and configuration validation.
//!
//! This is the stateless placement contract: source tokens are actual cached
//! content, target tokens are content to preserve, and only insertions made by
//! this analysis count as padding. There is no source layout or carried-pad
//! provenance.

use crate::cache_view::CacheIdentity;

/// Default relative loss: 1000 basis points (10%).
pub const DEFAULT_LOSS_BASIS_POINTS: u16 = 1_000;

/// Relative completion-loss allowance in basis points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LossAllowance {
    basis_points: u16,
}

impl LossAllowance {
    /// Construct from basis points. Values above 10000 are rejected by policy
    /// validation during analysis, not silently clamped.
    pub const fn new(basis_points: u16) -> Self {
        Self { basis_points }
    }

    pub const fn basis_points(self) -> u16 {
        self.basis_points
    }

    pub const fn percent(self) -> f64 {
        self.basis_points as f64 / 100.0
    }
}

impl Default for LossAllowance {
    fn default() -> Self {
        Self::new(DEFAULT_LOSS_BASIS_POINTS)
    }
}

/// Hard cap for generated placement variants.
pub const MAX_PLACEMENT_VARIANTS: usize = 4096;
/// Hard cap for current-call inserted padding, bounding score buckets.
pub const MAX_NEW_PADDING: u32 = 4096;
/// Modeled placement state/recipe payload ceiling. The search accounts retained
/// variants, order-index `StateKey` entries, state/bucket plans and insertion
/// capacities; allocator, `HashMap` table and spare `Vec` overhead are excluded.
pub const MAX_PLACEMENT_BYTES: u64 = 32 * 1024 * 1024;

/// Caller-declared placement constraints. All tokens are actual content; no
/// token value is interpreted as padding.
#[derive(Debug, Clone, Copy)]
pub struct PlacementPolicy<'a> {
    pub loss_allowance: LossAllowance,
    pub protected_prefix_tokens: u32,
    pub fresh_suffix_tokens: u32,
    /// Insert-before-target-boundary indices, sorted and unique.
    pub allowed_insertions: &'a [u32],
    /// Model-visible token used for this call's artificial insertions. `None`
    /// forbids them.
    pub pad_token: Option<u32>,
    pub source_start_alignment: u32,
    pub prepared_target_start_alignment: u32,
    pub reuse_count_alignment: u32,
    /// Smallest prepared-target position at which a reused span may start.
    pub minimum_prepared_reuse_start: u32,
    /// Smallest actual source count for a selected range.
    pub minimum_physical_reuse_tokens: u32,
    /// Maximum total original target length plus inserted padding.
    pub max_prepared_tokens: usize,
    pub max_new_padding: u32,
    /// Explicit bounded trim rectangle per maximal match. Derivation polls the
    /// deadline/cancellation on every nested iteration; the variant budget is
    /// the only truncation rule, not an alignment-only heuristic.
    pub max_trim_left: u32,
    pub max_trim_right: u32,
    pub max_variants: usize,
}

impl<'a> PlacementPolicy<'a> {
    /// Defaults for a no-insertion analysis whose only capacity is the supplied
    /// original target length.
    pub fn default_for(context_len: usize) -> Self {
        Self {
            loss_allowance: LossAllowance::default(),
            protected_prefix_tokens: 0,
            fresh_suffix_tokens: 0,
            allowed_insertions: &[],
            pad_token: None,
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

    /// Defaults with the general input-envelope ceiling. Prefer
    /// [`Self::default_for`] when the target length is known.
    pub fn default_wide() -> Self {
        let mut policy = Self::default_for(0);
        policy.max_prepared_tokens = crate::discovery::MAX_DISCOVERY_TOKENS;
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
        let mut previous = None;
        for &boundary in self.allowed_insertions {
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
    pub source_start: u32,
    pub destination_start: u32,
    pub token_count: u32,
    pub prepared_start: u32,
}

impl PlacedRange {
    pub fn source_end(self) -> u32 {
        self.source_start + self.token_count
    }

    pub fn destination_end(self) -> u32 {
        self.destination_start + self.token_count
    }

    pub fn prepared_end(self) -> u32 {
        self.prepared_start + self.token_count
    }
}

/// Current-call insertion before an original target boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Insertion {
    pub before_target_index: u32,
    pub token: u32,
    pub count: u32,
}

/// Complete placement recipe. There is no source-provenance record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementRecipe {
    pub cache_identity: CacheIdentity,
    pub original_tokens: u32,
    pub prepared_tokens: u32,
    pub ranges: Vec<PlacedRange>,
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

    /// Materialize the prepared target from the caller's own original target
    /// and cache tokens. It validates exact copied content; it does not prove
    /// native execution or model-state compatibility.
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
    pub useful_tokens: u64,
    pub inserted_padding: u32,
    pub prepared_tokens: u32,
    pub fresh_tokens: u32,
    pub best_useful: u64,
    pub admissible_min: u64,
    pub loss_allowance: LossAllowance,
}

/// Completeness/limit status for a returned plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacementStatus {
    /// All declared variants and canonical placements were explored.
    Complete,
    /// No non-empty placement was feasible; the deliberate empty plan is
    /// returned after complete exploration.
    NoFeasibleReuse,
    /// Search stopped at the deadline after publishing a valid plan.
    DeadlinePartial,
    /// The declared variant budget was reached.
    VariantLimitReached,
    /// The modeled placement payload was reached.
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
        let mut policy = PlacementPolicy::default_for(4);
        assert!(policy.validate(4).is_ok());
        policy.loss_allowance = LossAllowance::new(10_001);
        assert!(matches!(
            policy.validate(4),
            Err(PlacementConfigError::InvalidLossAllowance { .. })
        ));
        let mut policy = PlacementPolicy::default_for(4);
        policy.max_new_padding = 1;
        assert_eq!(
            policy.validate(4),
            Err(PlacementConfigError::MissingPadToken)
        );
        let mut policy = PlacementPolicy::default_for(4);
        policy.allowed_insertions = &[2, 1];
        assert!(matches!(
            policy.validate(4),
            Err(PlacementConfigError::UnsortedInsertions { .. })
        ));
        let mut policy = PlacementPolicy::default_for(4);
        policy.max_prepared_tokens = 3;
        assert!(matches!(
            policy.validate(4),
            Err(PlacementConfigError::InvalidCapacity { .. })
        ));
        let mut policy = PlacementPolicy::default_for(4);
        policy.max_new_padding = MAX_NEW_PADDING + 1;
        policy.pad_token = Some(1);
        assert!(matches!(
            policy.validate(4),
            Err(PlacementConfigError::PaddingLimitTooLarge { .. })
        ));
        let mut policy = PlacementPolicy::default_for(4);
        policy.max_variants = MAX_PLACEMENT_VARIANTS + 1;
        assert!(matches!(
            policy.validate(4),
            Err(PlacementConfigError::InvalidVariantLimit { .. })
        ));
        let mut policy = PlacementPolicy::default_for(4);
        policy.protected_prefix_tokens = 3;
        policy.fresh_suffix_tokens = 2;
        assert_eq!(
            policy.validate(4),
            Err(PlacementConfigError::PrefixSuffixOutOfRange)
        );
        let mut policy = PlacementPolicy::default_for(4);
        policy.allowed_insertions = &[5];
        assert!(matches!(
            policy.validate(4),
            Err(PlacementConfigError::InsertionBoundaryOutOfRange { .. })
        ));
        let mut policy = PlacementPolicy::default_for(4);
        policy.source_start_alignment = 0;
        assert_eq!(policy.validate(4), Err(PlacementConfigError::ZeroAlignment));
    }

    #[test]
    fn loss_allowance_floor_examples_are_integer_and_documented() {
        let floor = |best: u64, bp: u16| best - best.saturating_mul(bp as u64) / 10_000;
        assert_eq!(floor(980, 1_000), 882);
        assert_eq!(floor(200, 1_000), 180);
        assert_eq!(floor(199, 1_000), 180);
        assert_eq!(floor(1, 1_000), 1);
        assert_eq!(floor(980, 10_000), 0);
    }
}
