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
/// # use scr_planner::LossAllowance;
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
}

impl Default for LossAllowance {
    fn default() -> Self {
        Self::from_percent(10)
    }
}

/// Provisional cap on generated placement variants.
///
/// This bounds trim-variant derivation and retained candidate work. It is an
/// engineering bound, not a calibrated percentile of real workloads; real
/// history measurement is still pending. A plan that reaches it is returned
/// with [`PlacementStatus::VariantLimitReached`] when an incumbent exists, or
/// [`crate::PlanError::PlacementIncomplete`] when no plan exists.
pub const MAX_PLACEMENT_VARIANTS: usize = 4096;

/// Provisional policy cap for this call's inserted padding.
///
/// Padding is charged into score buckets, so this also bounds bucket-table
/// footprint. It is not a calibrated workload percentile. This is a policy
/// constraint, not an exploration-truncation limit: a caller's
/// `max_new_padding` above the cap is an invalid policy, and a candidate that
/// needs more than the declared/available padding is simply outside the
/// declared feasible domain. That can yield a valid smaller plan, a complete
/// `NoFeasibleReuse` empty plan, or no candidate; it does **not** by itself
/// produce `PlacementIncomplete`. Variant and modeled-state limits own partial
/// statuses and no-incumbent incomplete refusals.
pub const MAX_NEW_PADDING: u32 = 4096;

/// Modeled placement state/recipe payload ceiling.
///
/// The search accounts retained variants, order-index `StateKey` entries,
/// state/bucket plans and insertion capacities. Allocator overhead, the
/// `HashMap` table and spare `Vec` capacity are excluded. This is a modeled
/// planner-payload cap, not measured whole-process resident memory and not a
/// GPU-allocation promise. Reaching it yields a truthful
/// [`PlacementStatus::StateLimitReached`] incumbent or an incomplete refusal,
/// never an invented empty plan.
pub(crate) const MAX_PLACEMENT_BYTES: u64 = 32 * 1024 * 1024;

/// Caller-declared placement constraints. All tokens are actual content; no
/// token value is interpreted as padding.
///
/// Use [`Self::without_padding`] for an intentional no-insertion analysis, or
/// [`Self::with_padding`] when a caller supplies a pad token, allowed insertion
/// boundaries and prepared capacity. Fields remain public for adapter-specific
/// policy refinements such as native alignment.
#[derive(Debug, Clone, Copy)]
pub struct PlacementPolicy<'a> {
    /// Relative useful-coverage loss allowed when choosing between plans.
    pub loss_allowance: LossAllowance,
    /// Leading original-target tokens that must remain fresh.
    pub protected_prefix_tokens: u32,
    /// Trailing original-target tokens that must remain fresh.
    pub fresh_suffix_tokens: u32,
    /// Original-target boundaries where insertion is permitted, sorted and
    /// unique. An empty slice permits none unless the whole boundary space is
    /// explicitly enabled by the caller.
    pub allowed_insertions: &'a [u32],
    /// Model-visible token for this call's insertions. `None` forbids padding.
    pub pad_token: Option<u32>,
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
    /// Explicit bounded trim rectangle per maximal match. Derivation polls the
    /// deadline/cancellation on every nested iteration; the variant budget is
    /// the only truncation rule, not an alignment-only heuristic.
    pub max_trim_left: u32,
    pub max_trim_right: u32,
    pub max_variants: usize,
}

impl<'a> PlacementPolicy<'a> {
    /// Construct a no-padding policy for `context_len`.
    ///
    /// It forbids insertions, permits no insertion boundaries and caps prepared
    /// tokens at the original target length. Alignments are one and no trimming
    /// is requested.
    pub fn without_padding(context_len: usize) -> Self {
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

    /// Construct a padding-capable policy with caller-owned inputs.
    ///
    /// `pad_token` is the model-visible token for this call's insertions.
    /// `allowed_insertions` contains the original-target boundaries where the
    /// caller permits insertion; it must be sorted and unique. `prepared_capacity`
    /// is the total original-target-plus-padding capacity. The derived
    /// `max_new_padding` is the spare capacity, capped by [`MAX_NEW_PADDING`].
    ///
    /// Both constructors start with neutral alignment 1. A model that requires
    /// a different alignment sets the public alignment fields explicitly. The
    /// constructor does not choose a token or permit boundaries outside the
    /// supplied slice. Invalid capacity or boundaries are rejected during
    /// analysis.
    ///
    /// ```
    /// # use scr_planner::PlacementPolicy;
    /// let policy = PlacementPolicy::with_padding(10, 99, &[0, 1], 11);
    /// assert_eq!(policy.pad_token, Some(99));
    /// assert_eq!(policy.max_new_padding, 1);
    /// assert_eq!(policy.max_prepared_tokens, 11);
    /// assert_eq!(policy.prepared_target_start_alignment, 1);
    /// ```
    pub fn with_padding(
        context_len: usize,
        pad_token: u32,
        allowed_insertions: &'a [u32],
        prepared_capacity: usize,
    ) -> Self {
        let max_new_padding = prepared_capacity
            .saturating_sub(context_len)
            .min(MAX_NEW_PADDING as usize) as u32;
        let mut policy = Self::without_padding(context_len);
        policy.pad_token = Some(pad_token);
        policy.allowed_insertions = allowed_insertions;
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

    /// Build and allocate the prepared target token buffer.
    ///
    /// The method walks the original target, inserts this recipe's pads at their
    /// declared boundaries, and copies reused spans from the borrowed source.
    /// Every copied reused token must equal the original target token at that
    /// position; a mismatch or malformed recipe is returned as a typed
    /// [`PlacementMaterializeError`]. The result is a new `Vec<u32>` owned by the
    /// caller.
    ///
    /// This is a token-buffer materialization helper only. It does not install
    /// or copy native cache tensors, validate model state, or run inference;
    /// native execution remains an adapter responsibility. `context` is the
    /// original target, not the prepared target. The returned buffer adds
    /// `4 * (original_tokens + inserted_padding)` bytes of token payload; the
    /// original target/source remain borrowed and live, while native KV/cache
    /// tensors are separate. That is not a total peak-memory or RSS claim.
    ///
    /// ```
    /// # use scr_planner::{CacheIdentity, Insertion, PlacedRange, PlacementRecipe};
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

/// Why a caller-constructed or caller-modified placement recipe was rejected.
///
/// `RecipeShape` is the caller-facing structural error; `InputShape`,
/// `SourceRange`, `ContentMismatch` and `Allocation` describe materialization
/// inputs or allocation. These are not planner invariants: the planner's own
/// recipe is checked separately before a `ReusePlan` is returned.
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

/// Completeness/limit status for a returned plan.
///
/// A status describes what the planner actually completed. A valid incumbent
/// is still returned with `DeadlinePartial`, `VariantLimitReached` or
/// `StateLimitReached`; if no incumbent exists, analysis returns an informative
/// error instead of an empty success. Deadline expiry is cooperative and does
/// not by itself bound memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacementStatus {
    /// All declared variants and canonical placements were explored.
    Complete,
    /// No non-empty placement was feasible; the deliberate empty plan is
    /// returned after complete exploration.
    NoFeasibleReuse,
    /// Search stopped at the deadline after publishing a valid plan. Further
    /// variants/states were not explored.
    DeadlinePartial,
    /// The provisional variant budget was reached, so some declared trim
    /// variants or retained candidates were not derived/searched. A valid
    /// incumbent, if present, is returned with this status; otherwise analysis
    /// returns [`crate::PlanError::PlacementIncomplete`].
    VariantLimitReached,
    /// The modeled placement payload was reached, so some state/extension work
    /// was not admitted. A valid incumbent, if present, is returned with this
    /// status; otherwise analysis returns [`crate::PlanError::PlacementIncomplete`].
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
        policy.allowed_insertions = &[2, 1];
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
        policy.allowed_insertions = &[5];
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
        let floor = |best: u64, bp: u16| best - best.saturating_mul(bp as u64) / 10_000;
        assert_eq!(floor(980, 1_000), 882);
        assert_eq!(floor(200, 1_000), 180);
        assert_eq!(floor(199, 1_000), 180);
        assert_eq!(floor(1, 1_000), 1);
        assert_eq!(floor(980, 10_000), 0);
    }
}
