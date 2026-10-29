//! Materializing a placement recipe into caller-owned buffers.

use cache_reuse_utils::{
    AllowedInsertions, CacheIdentity, CacheReuseAnalysis, CacheView, Insertion, LossAllowance,
    PlacedRange, PlacementMaterializeError, PlacementPolicy, PlacementRecipe, ReusePlan,
    project_to_logical,
};
use std::time::{Duration, Instant};

const PAD: u32 = 99;

/// Two cached blocks that each start one token past a multiple of four, so
/// full coverage at aligned prepared starts needs a pad run before each.
fn two_padded_blocks() -> (Vec<u32>, Vec<u32>, ReusePlan) {
    let mut context = vec![100];
    context.extend(1..=8);
    context.push(101);
    context.extend(11..=18);
    let mut cache: Vec<u32> = (1..=8).collect();
    cache.push(50);
    cache.extend(11..=18);
    let mut policy = PlacementPolicy::with_padding(context.len(), PAD, AllowedInsertions::All, 32);
    policy.prepared_target_start_alignment = 4;
    policy.loss_allowance = LossAllowance::from_percent(0);
    policy.pad_penalty_tokens = 0;
    policy.range_penalty_tokens = 1;
    let mut cancel = || false;
    let plan = CacheReuseAnalysis {
        context_tokens: &context,
        cache: CacheView {
            tokens: &cache,
            identity: CacheIdentity::new(1),
            minimum_match_tokens: 4,
        },
        k: 4,
        deadline: Instant::now() + Duration::from_secs(30),
        cancel: &mut cancel,
        placement: policy,
    }
    .analyze()
    .expect("plan");
    (context, cache, plan)
}

#[test]
fn planner_insertions_record_their_prepared_start() {
    let (_, _, plan) = two_padded_blocks();
    let insertions = &plan.placement().insertions;
    assert_eq!(insertions.len(), 2, "{insertions:?}");
    let mut prior = 0;
    for insertion in insertions {
        assert_eq!(
            insertion.prepared_start,
            insertion.before_target_index + prior
        );
        prior += insertion.count;
    }
}

#[test]
fn materialize_into_writes_the_prepared_target_and_pad_positions() {
    let (context, cache, plan) = two_padded_blocks();
    let recipe = plan.placement();
    let mut prepared = vec![0; recipe.prepared_tokens as usize];
    let mut pads = vec![0; (recipe.prepared_tokens - recipe.original_tokens) as usize];
    recipe
        .materialize_into(&context, &cache, &mut prepared, Some(&mut pads))
        .expect("materialize_into");

    assert_eq!(prepared, recipe.materialize(&context, &cache).unwrap());
    let expected_pads: Vec<u32> = recipe
        .insertions
        .iter()
        .flat_map(|insertion| insertion.prepared_start..insertion.prepared_start + insertion.count)
        .collect();
    assert_eq!(pads, expected_pads);
    let mut logical = vec![0; context.len()];
    assert_eq!(
        project_to_logical(&prepared, &pads, &mut logical),
        Some(context.len() as u32)
    );
    assert_eq!(logical, context);
    for (index, &token) in prepared.iter().enumerate() {
        assert_eq!(
            token == PAD,
            pads.contains(&(index as u32)),
            "index {index}"
        );
    }
    for range in &recipe.ranges {
        let prepared_span = &prepared[range.prepared_start as usize..range.prepared_end() as usize];
        assert_eq!(
            prepared_span,
            &cache[range.source_start as usize..range.source_end() as usize]
        );
        assert_eq!(range.prepared_start % 4, 0);
    }
}

/// `[0, 9, 1, 2, 3]` from context `[0, 1, 2, 3]` with source `[1, 2]`.
fn one_pad_recipe() -> PlacementRecipe {
    PlacementRecipe {
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
            prepared_start: 1,
            token: 9,
            count: 1,
        }],
    }
}

#[test]
fn materialize_into_without_pad_positions() {
    let mut prepared = [0; 5];
    one_pad_recipe()
        .materialize_into(&[0, 1, 2, 3], &[1, 2], &mut prepared, None)
        .unwrap();
    assert_eq!(prepared, [0, 9, 1, 2, 3]);
}

#[test]
fn materialize_into_rejects_buffers_of_the_wrong_length() {
    let recipe = one_pad_recipe();
    let context = [0, 1, 2, 3];
    let source = [1, 2];
    for (context_len, prepared_len, pad_len) in
        [(3, 5, 1), (4, 4, 1), (4, 6, 1), (4, 5, 0), (4, 5, 2)]
    {
        let mut prepared = vec![0; prepared_len];
        let mut pads = vec![0; pad_len];
        assert_eq!(
            recipe.materialize_into(
                &context[..context_len.min(4)],
                &source,
                &mut prepared,
                Some(&mut pads)
            ),
            Err(PlacementMaterializeError::InputShape),
            "{context_len} {prepared_len} {pad_len}"
        );
    }
}

#[test]
fn materialize_into_verifies_copied_content() {
    let recipe = one_pad_recipe();
    let mut prepared = [0; 5];
    assert_eq!(
        recipe.materialize_into(&[0, 1, 2, 3], &[1, 7], &mut prepared, None),
        Err(PlacementMaterializeError::ContentMismatch { index: 2 })
    );
    assert_eq!(
        recipe.materialize_into(&[0, 1, 2, 3], &[1], &mut prepared, None),
        Err(PlacementMaterializeError::SourceRange)
    );
}

#[test]
fn validate_rejects_an_inconsistent_insertion_prepared_start() {
    for prepared_start in [0, 2] {
        let mut recipe = one_pad_recipe();
        recipe.insertions[0].prepared_start = prepared_start;
        assert_eq!(
            recipe.validate(),
            Err(PlacementMaterializeError::RecipeShape)
        );
        assert_eq!(
            recipe.materialize(&[0, 1, 2, 3], &[1, 2]),
            Err(PlacementMaterializeError::RecipeShape)
        );
    }
}

#[test]
fn a_second_insertion_starts_after_the_first() {
    let recipe = PlacementRecipe {
        cache_identity: CacheIdentity::new(1),
        original_tokens: 2,
        prepared_tokens: 5,
        ranges: vec![],
        insertions: vec![
            Insertion {
                before_target_index: 0,
                prepared_start: 0,
                token: 8,
                count: 2,
            },
            Insertion {
                before_target_index: 2,
                prepared_start: 4,
                token: 9,
                count: 1,
            },
        ],
    };
    let mut prepared = [0; 5];
    let mut pads = [0; 3];
    recipe
        .materialize_into(&[1, 2], &[], &mut prepared, Some(&mut pads))
        .unwrap();
    assert_eq!(prepared, [8, 8, 1, 2, 9]);
    assert_eq!(pads, [0, 1, 4]);

    let mut stale = recipe.clone();
    // Missing the first run's two tokens.
    stale.insertions[1].prepared_start = 2;
    assert_eq!(
        stale.validate(),
        Err(PlacementMaterializeError::RecipeShape)
    );
}
