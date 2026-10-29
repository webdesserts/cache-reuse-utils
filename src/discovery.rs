//! Exact maximal-match discovery over one cache source.

use crate::cache_view::CacheView;
use crate::candidate::{CandidateRange, MAX_CANDIDATES};
use crate::error::{AdmissionDeniedReason, PlanErrorKind};
use crate::timing::Stopper;

/// Each input is capped at 1,048,576 tokens to bound index time and memory.
/// This is a planner limit, not a native execution limit.
pub(crate) const MAX_DISCOVERY_TOKENS: usize = 1 << 20;

const HASH_BASE: u64 = 257;

/// Whether discovery considered every qualifying maximal match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryStatus {
    /// All qualifying maximal matches were considered.
    Complete,
    /// More qualifying matches existed than the candidate capacity.
    CandidateLimitReached,
}

#[derive(Debug, Clone, Copy)]
struct Seed {
    hash: u64,
    source: u32,
}

pub(crate) struct DiscoveryOutcome {
    pub ranges: Vec<CandidateRange>,
    pub status: DiscoveryStatus,
}

fn seed_key(seed: &Seed) -> (u64, u32) {
    (seed.hash, seed.source)
}

fn window_hash(
    tokens: &[u32],
    start: usize,
    minimum: usize,
    stopper: &mut Stopper<'_>,
) -> Result<u64, PlanErrorKind> {
    let mut hash = 0u64;
    for token in &tokens[start..start + minimum] {
        stopper.tick()?;
        hash = hash.wrapping_mul(HASH_BASE).wrapping_add(*token as u64 + 1);
    }
    Ok(hash)
}

fn update_hash(hash: u64, outgoing: u32, incoming: u32, power: u64) -> u64 {
    hash.wrapping_mul(HASH_BASE)
        .wrapping_add(incoming as u64 + 1)
        .wrapping_sub((outgoing as u64 + 1).wrapping_mul(power))
}

/// Sort seeds by `(hash, source)`, checking for a stop while partitioning.
fn sort_seeds(seeds: &mut [Seed], stopper: &mut Stopper<'_>) -> Result<(), PlanErrorKind> {
    fn insertion(slice: &mut [Seed], stopper: &mut Stopper<'_>) -> Result<(), PlanErrorKind> {
        for i in 1..slice.len() {
            let mut j = i;
            while j > 0 && seed_key(&slice[j - 1]) > seed_key(&slice[j]) {
                slice.swap(j - 1, j);
                j -= 1;
                stopper.tick()?;
            }
            stopper.tick()?;
        }
        Ok(())
    }

    fn quicksort(mut slice: &mut [Seed], stopper: &mut Stopper<'_>) -> Result<(), PlanErrorKind> {
        const SMALL: usize = 24;
        loop {
            if slice.len() <= SMALL {
                return insertion(slice, stopper);
            }
            let mid = slice.len() / 2;
            let pivot = seed_key(&slice[mid]);
            let mut less = 0;
            let mut equal = 0;
            let mut greater = slice.len();
            while equal < greater {
                let key = seed_key(&slice[equal]);
                if key < pivot {
                    slice.swap(less, equal);
                    less += 1;
                    equal += 1;
                } else if key > pivot {
                    greater -= 1;
                    slice.swap(equal, greater);
                } else {
                    equal += 1;
                }
                stopper.tick()?;
            }
            // Recurse into the smaller partition and loop over the larger one,
            // so recursion depth is O(log N).
            if less <= slice.len() - greater {
                quicksort(&mut slice[..less], stopper)?;
                slice = &mut slice[greater..];
            } else {
                quicksort(&mut slice[greater..], stopper)?;
                slice = &mut slice[..less];
            }
        }
    }

    quicksort(seeds, stopper)
}

fn exact_prefix(
    source: &[u32],
    source_start: usize,
    target: &[u32],
    target_start: usize,
    minimum: usize,
    stopper: &mut Stopper<'_>,
) -> Result<bool, PlanErrorKind> {
    for offset in 0..minimum {
        stopper.tick()?;
        if source[source_start + offset] != target[target_start + offset] {
            return Ok(false);
        }
    }
    Ok(true)
}

fn extend_match(
    source: &[u32],
    source_start: usize,
    target: &[u32],
    target_start: usize,
    minimum: usize,
    stopper: &mut Stopper<'_>,
) -> Result<(usize, usize, usize), PlanErrorKind> {
    let mut left = 0usize;
    while source_start > left && target_start > left {
        stopper.tick()?;
        if source[source_start - left - 1] != target[target_start - left - 1] {
            break;
        }
        left += 1;
    }
    let mut right = minimum;
    while source_start + right < source.len() && target_start + right < target.len() {
        stopper.tick()?;
        if source[source_start + right] != target[target_start + right] {
            break;
        }
        right += 1;
    }
    Ok((source_start - left, target_start - left, left + right))
}

/// Discover every maximal exact match at least `cache.minimum_match_tokens`
/// long.
///
/// Every source and target position is a seed. Hash equality is only a
/// filter: each seed pair is verified token by token. A per-diagonal coverage
/// end skips windows inside an already-found maximal run. The caller has
/// already rejected inputs that cannot match.
pub(crate) fn discover(
    context: &[u32],
    cache: CacheView<'_>,
    stopper: &mut Stopper<'_>,
) -> Result<DiscoveryOutcome, PlanErrorKind> {
    let minimum = cache.minimum_match_tokens as usize;
    let source = cache.tokens;
    let source_windows = source.len() - minimum + 1;
    let covered_len = source.len() + context.len() + 1;

    let mut seeds = Vec::new();
    seeds
        .try_reserve_exact(source_windows)
        .map_err(|_| PlanErrorKind::AdmissionDenied(AdmissionDeniedReason::Allocation))?;
    let mut covered = Vec::new();
    covered
        .try_reserve_exact(covered_len)
        .map_err(|_| PlanErrorKind::AdmissionDenied(AdmissionDeniedReason::Allocation))?;
    let mut ranges = Vec::new();

    let mut power = 1u64;
    for _ in 0..minimum {
        power = power.wrapping_mul(HASH_BASE);
    }
    let mut hash = window_hash(source, 0, minimum, stopper)?;
    for i in 0..source_windows {
        stopper.tick()?;
        if i > 0 {
            hash = update_hash(hash, source[i - 1], source[i + minimum - 1], power);
        }
        seeds.push(Seed {
            hash,
            source: i as u32,
        });
    }
    sort_seeds(&mut seeds, stopper)?;

    for _ in 0..covered_len {
        stopper.tick()?;
        covered.push(0u32);
    }

    let mut target_hash = window_hash(context, 0, minimum, stopper)?;
    let target_windows = context.len() - minimum + 1;
    for destination in 0..target_windows {
        stopper.tick()?;
        if destination > 0 {
            target_hash = update_hash(
                target_hash,
                context[destination - 1],
                context[destination + minimum - 1],
                power,
            );
        }
        let mut pair = seeds.partition_point(|seed| seed.hash < target_hash);
        while pair < seeds.len() && seeds[pair].hash == target_hash {
            stopper.tick()?;
            let source_start = seeds[pair].source as usize;
            let diagonal = source_start + context.len() - destination;
            if covered[diagonal] < (destination + minimum) as u32
                && exact_prefix(source, source_start, context, destination, minimum, stopper)?
            {
                let (start, begin, count) =
                    extend_match(source, source_start, context, destination, minimum, stopper)?;
                covered[diagonal] = (begin + count) as u32;
                if ranges.len() == MAX_CANDIDATES {
                    return Ok(DiscoveryOutcome {
                        ranges,
                        status: DiscoveryStatus::CandidateLimitReached,
                    });
                }
                ranges.push(CandidateRange {
                    source_start: start as u32,
                    destination_start: begin as u32,
                    token_count: count as u32,
                });
            }
            pair += 1;
        }
    }
    Ok(DiscoveryOutcome {
        ranges,
        status: DiscoveryStatus::Complete,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache_view::{CacheIdentity, CacheView};
    use std::collections::BTreeSet;
    use std::time::{Duration, Instant};

    fn discover_ranges(source: &[u32], target: &[u32], minimum: u32) -> Vec<CandidateRange> {
        let mut cancel = || false;
        let mut stopper = Stopper::new(Instant::now() + Duration::from_secs(5), &mut cancel);
        let cache = CacheView {
            tokens: source,
            identity: CacheIdentity::new(1),
            minimum_match_tokens: minimum,
        };
        discover(target, cache, &mut stopper)
            .expect("discovery")
            .ranges
    }

    fn maximal_oracle(source: &[u32], target: &[u32], minimum: usize) -> BTreeSet<(u32, u32, u32)> {
        let mut matches = BTreeSet::new();
        for source_start in 0..source.len() {
            for target_start in 0..target.len() {
                if source[source_start] != target[target_start] {
                    continue;
                }
                if source_start > 0
                    && target_start > 0
                    && source[source_start - 1] == target[target_start - 1]
                {
                    continue;
                }
                let mut count = 0usize;
                while source_start + count < source.len()
                    && target_start + count < target.len()
                    && source[source_start + count] == target[target_start + count]
                {
                    count += 1;
                }
                if count >= minimum {
                    matches.insert((source_start as u32, target_start as u32, count as u32));
                }
            }
        }
        matches
    }

    #[test]
    fn maximal_matches_are_exact_and_complete_on_small_inputs() {
        let mut state = 0x9e37_79b9u64;
        let mut next = || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((state >> 33) % 5) as u32
        };
        for case in 0..80 {
            let source_len = 1 + (case % 7);
            let target_len = 1 + ((case / 7) % 7);
            let source: Vec<u32> = (0..source_len).map(|_| next()).collect();
            let target: Vec<u32> = (0..target_len).map(|_| next()).collect();
            let minimum = 1 + (case % 3) as u32;
            if minimum as usize > source_len || minimum as usize > target_len {
                continue;
            }
            let actual: BTreeSet<_> = discover_ranges(&source, &target, minimum)
                .into_iter()
                .map(|r| (r.source_start, r.destination_start, r.token_count))
                .collect();
            let expected = maximal_oracle(&source, &target, minimum as usize);
            assert_eq!(actual, expected, "case {case}");
        }
    }

    #[test]
    fn seed_sort_stops_on_cancellation() {
        let base = Instant::now();
        let mut seeds: Vec<Seed> = (0..5_000u32)
            .rev()
            .map(|source| Seed {
                hash: source as u64,
                source,
            })
            .collect();
        let mut cancel = || true;
        let mut stopper =
            Stopper::with_clock(base + Duration::from_secs(5), &mut cancel, move |_| base);
        assert_eq!(
            sort_seeds(&mut seeds, &mut stopper),
            Err(PlanErrorKind::Cancelled)
        );
    }

    #[test]
    fn odd_positions_are_not_filtered_by_physical_parity() {
        let ranges = discover_ranges(&[9, 1, 2, 3], &[8, 9, 1, 2, 3], 3);
        assert_eq!(
            ranges,
            vec![CandidateRange {
                source_start: 0,
                destination_start: 1,
                token_count: 4,
            }]
        );
    }
}
