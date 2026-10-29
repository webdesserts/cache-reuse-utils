//! Randomized checks against independent oracles.

use cache_reuse_utils::{
    AllowedInsertions, CacheIdentity, CacheReuseAnalysis, CacheView, LossAllowance,
    PlacementPolicy, ReusePlan,
};
use std::time::{Duration, Instant};

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u32) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) % n as u64) as u32
    }

    fn tokens(&mut self, max_len: u32, alphabet: u32) -> Vec<u32> {
        (0..1 + self.below(max_len))
            .map(|_| self.below(alphabet))
            .collect()
    }
}

fn analyze(context: &[u32], cache: &[u32], k: u32, policy: PlacementPolicy<'_>) -> ReusePlan {
    let mut cancel = || false;
    CacheReuseAnalysis {
        context_tokens: context,
        cache: CacheView {
            tokens: cache,
            identity: CacheIdentity::new(1),
            minimum_match_tokens: 1,
        },
        k,
        deadline: Instant::now() + Duration::from_secs(30),
        cancel: &mut cancel,
        placement: policy,
    }
    .analyze()
    .expect("plan")
}

/// Most tokens coverable by at most `k` ranges that increase in both
/// sequences and stay inside `window`, by exhaustive memoized search over
/// every exact sub-range.
fn best_coverage(source: &[u32], target: &[u32], window: (usize, usize), k: usize) -> u64 {
    struct Oracle<'a> {
        source: &'a [u32],
        target: &'a [u32],
        window: (usize, usize),
        k: usize,
        memo: Vec<Option<u64>>,
    }

    impl Oracle<'_> {
        /// Best coverage from target `t0` and source `s0` with `left` ranges.
        fn best(&mut self, t0: usize, s0: usize, left: usize) -> u64 {
            let index = (t0 * (self.source.len() + 1) + s0) * (self.k + 1) + left;
            if let Some(value) = self.memo[index] {
                return value;
            }
            let mut best = 0;
            if left > 0 {
                for t in t0.max(self.window.0)..self.window.1 {
                    for s in s0..self.source.len() {
                        let mut n = 0;
                        while t + n < self.window.1
                            && s + n < self.source.len()
                            && self.target[t + n] == self.source[s + n]
                        {
                            n += 1;
                            best = best.max(n as u64 + self.best(t + n, s + n, left - 1));
                        }
                    }
                }
            }
            self.memo[index] = Some(best);
            best
        }
    }

    let size = (target.len() + 1) * (source.len() + 1) * (k + 1);
    let mut oracle = Oracle {
        source,
        target,
        window,
        k,
        memo: vec![None; size],
    };
    oracle.best(0, 0, k)
}

#[test]
fn search_matches_an_exhaustive_oracle_without_padding() {
    let mut rng = Rng(7);
    for case in 0..400 {
        let context = rng.tokens(9, 3);
        let cache = rng.tokens(9, 3);
        let k = 1 + rng.below(3);
        let prefix = rng.below(2).min(context.len() as u32);
        let suffix = rng.below(2).min(context.len() as u32 - prefix);
        let mut policy = PlacementPolicy::without_padding(context.len());
        policy.loss_allowance = LossAllowance::from_percent(0);
        policy.protected_prefix_tokens = prefix;
        policy.fresh_suffix_tokens = suffix;
        let plan = analyze(&context, &cache, k, policy);
        let window = (prefix as usize, context.len() - suffix as usize);
        assert_eq!(
            plan.covered_tokens(),
            best_coverage(&cache, &context, window, k as usize),
            "case {case}: context {context:?} cache {cache:?} k {k} window {window:?}"
        );
        assert!(plan.ranges().len() <= k as usize);
        assert_eq!(
            plan.placement().materialize(&context, &cache).unwrap(),
            context
        );
    }
}

#[test]
fn padded_plans_materialize_and_honor_the_policy() {
    let mut rng = Rng(11);
    let boundaries: Vec<u32> = (0..=12).collect();
    for case in 0..400 {
        let context = rng.tokens(12, 3);
        let cache = rng.tokens(12, 3);
        let spare = rng.below(5) as usize;
        let allowed = if rng.below(2) == 0 {
            AllowedInsertions::All
        } else {
            let count = rng.below(context.len() as u32 + 2) as usize;
            AllowedInsertions::Boundaries(&boundaries[..count])
        };
        let mut policy =
            PlacementPolicy::with_padding(context.len(), 99, allowed, context.len() + spare);
        policy.prepared_target_start_alignment = 1 + rng.below(3);
        policy.minimum_prepared_reuse_start = rng.below(3);
        policy.pad_penalty_tokens = rng.below(3);
        policy.loss_allowance = LossAllowance::from_percent(rng.below(40) as u8);
        let plan = analyze(&context, &cache, 1 + rng.below(3), policy);
        let recipe = plan.placement();
        let prepared = recipe.materialize(&context, &cache).unwrap();
        assert_eq!(
            prepared.len(),
            recipe.prepared_tokens as usize,
            "case {case}"
        );
        assert!(recipe.prepared_tokens as usize <= context.len() + spare);
        for range in &recipe.ranges {
            assert_eq!(
                range.prepared_start % policy.prepared_target_start_alignment,
                0
            );
            assert!(range.prepared_start >= policy.minimum_prepared_reuse_start);
            let reused = &prepared[range.prepared_start as usize..range.prepared_end() as usize];
            assert_eq!(
                reused,
                &cache[range.source_start as usize..range.source_end() as usize]
            );
        }
        let mut original = Vec::new();
        let mut position = 0;
        for (index, &token) in context.iter().enumerate() {
            for insertion in recipe
                .insertions
                .iter()
                .filter(|i| i.before_target_index as usize == index)
            {
                if let AllowedInsertions::Boundaries(allowed) = policy.allowed_insertions {
                    assert!(allowed.contains(&insertion.before_target_index));
                }
                assert!(
                    prepared[position..][..insertion.count as usize]
                        .iter()
                        .all(|&t| t == 99)
                );
                position += insertion.count as usize;
            }
            original.push(token);
            assert_eq!(prepared[position], token, "case {case}");
            position += 1;
        }
        assert_eq!(original, context);
    }
}
