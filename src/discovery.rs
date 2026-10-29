//! Exact maximal-match discovery over one cache source.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::ops::Range;

use crate::cache_view::CacheView;
use crate::candidate::{CandidateRange, MAX_CANDIDATES};
use crate::timing::{Stop, Stopper};

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

pub(crate) struct DiscoveryOutcome {
    pub ranges: Vec<CandidateRange>,
    pub status: DiscoveryStatus,
}

fn window_hash(tokens: &[u32], start: usize, minimum: usize) -> u64 {
    tokens[start..start + minimum]
        .iter()
        .fold(0u64, |hash, &token| {
            hash.wrapping_mul(HASH_BASE).wrapping_add(token as u64 + 1)
        })
}

fn update_hash(hash: u64, outgoing: u32, incoming: u32, power: u64) -> u64 {
    hash.wrapping_mul(HASH_BASE)
        .wrapping_add(incoming as u64 + 1)
        .wrapping_sub((outgoing as u64 + 1).wrapping_mul(power))
}

fn extend_match(
    source: &[u32],
    source_start: usize,
    target: &[u32],
    target_start: usize,
    minimum: usize,
    stopper: &mut Stopper<'_>,
) -> Result<(usize, usize, usize), Stop> {
    let mut left = 0usize;
    while source_start > left
        && target_start > left
        && source[source_start - left - 1] == target[target_start - left - 1]
    {
        stopper.tick()?;
        left += 1;
    }
    let mut right = minimum;
    while source_start + right < source.len()
        && target_start + right < target.len()
        && source[source_start + right] == target[target_start + right]
    {
        stopper.tick()?;
        right += 1;
    }
    Ok((source_start - left, target_start - left, left + right))
}

/// Candidate rank: longer is better, then earlier destination, then earlier
/// source. The outer `Reverse` puts the weakest retained candidate on top of
/// the max-heap.
type Rank = Reverse<(u32, Reverse<u32>, Reverse<u32>)>;

/// Retains the [`MAX_CANDIDATES`] best-ranked matches seen.
#[derive(Default)]
struct Longest {
    heap: BinaryHeap<Rank>,
    dropped: bool,
}

impl Longest {
    fn push(&mut self, range: CandidateRange) {
        let entry = Reverse((
            range.token_count,
            Reverse(range.destination_start),
            Reverse(range.source_start),
        ));
        if self.heap.len() < MAX_CANDIDATES {
            self.heap.push(entry);
            return;
        }
        self.dropped = true;
        if let Some(mut weakest) = self.heap.peek_mut()
            && entry < *weakest
        {
            *weakest = entry;
        }
    }

    fn into_outcome(self) -> DiscoveryOutcome {
        let mut ranges: Vec<CandidateRange> = self
            .heap
            .into_iter()
            .map(
                |Reverse((token_count, Reverse(destination_start), Reverse(source_start)))| {
                    CandidateRange {
                        source_start,
                        destination_start,
                        token_count,
                    }
                },
            )
            .collect();
        ranges.sort_unstable_by_key(|range| (range.destination_start, range.source_start));
        DiscoveryOutcome {
            ranges,
            status: if self.dropped {
                DiscoveryStatus::CandidateLimitReached
            } else {
                DiscoveryStatus::Complete
            },
        }
    }
}

/// Discover maximal exact matches at least `cache.minimum_match_tokens` long
/// inside `window` of the context, keeping the [`MAX_CANDIDATES`] longest.
///
/// Matches are maximal within the window: a match that crosses the protected
/// prefix or fresh suffix is clipped to the reusable part before ranking.
/// Every source position is a seed; hash equality is only a filter, and each
/// seed pair is verified token by token. A per-diagonal coverage end skips
/// windows inside an already-found run. Destinations are in full-context
/// coordinates.
pub(crate) fn discover(
    context: &[u32],
    window: Range<usize>,
    cache: CacheView<'_>,
    stopper: &mut Stopper<'_>,
) -> Result<DiscoveryOutcome, Stop> {
    let offset = window.start;
    let target = &context[window];
    let minimum = cache.minimum_match_tokens as usize;
    let source = cache.tokens;
    let mut longest = Longest::default();
    if minimum > target.len() || minimum > source.len() {
        return Ok(longest.into_outcome());
    }

    let power = (0..minimum).fold(1u64, |power, _| power.wrapping_mul(HASH_BASE));
    let mut seeds = Vec::with_capacity(source.len() - minimum + 1);
    let mut hash = window_hash(source, 0, minimum);
    for i in 0..=source.len() - minimum {
        stopper.tick()?;
        if i > 0 {
            hash = update_hash(hash, source[i - 1], source[i + minimum - 1], power);
        }
        seeds.push((hash, i as u32));
    }
    seeds.sort_unstable();
    stopper.check()?;

    // Diagonal `source - destination + target.len()` -> end of the last match
    // found on it, in target coordinates.
    let mut covered = vec![0u32; source.len() + target.len() + 1];
    let mut target_hash = window_hash(target, 0, minimum);
    for destination in 0..=target.len() - minimum {
        stopper.tick()?;
        if destination > 0 {
            target_hash = update_hash(
                target_hash,
                target[destination - 1],
                target[destination + minimum - 1],
                power,
            );
        }
        let first = seeds.partition_point(|&(hash, _)| hash < target_hash);
        for &(hash, source_start) in &seeds[first..] {
            if hash != target_hash {
                break;
            }
            stopper.tick()?;
            let source_start = source_start as usize;
            let diagonal = source_start + target.len() - destination;
            if covered[diagonal] >= (destination + minimum) as u32
                || source[source_start..source_start + minimum]
                    != target[destination..destination + minimum]
            {
                continue;
            }
            let (start, begin, count) =
                extend_match(source, source_start, target, destination, minimum, stopper)?;
            covered[diagonal] = (begin + count) as u32;
            longest.push(CandidateRange {
                source_start: start as u32,
                destination_start: (offset + begin) as u32,
                token_count: count as u32,
            });
        }
    }
    Ok(longest.into_outcome())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache_view::{CacheIdentity, CacheView};
    use std::collections::BTreeSet;
    use std::time::{Duration, Instant};

    type Found = BTreeSet<(u32, u32, u32)>;

    fn discover_in(source: &[u32], target: &[u32], window: Range<usize>, minimum: u32) -> Found {
        let mut cancel = || false;
        let mut stopper = Stopper::new(Instant::now() + Duration::from_secs(5), &mut cancel);
        let cache = CacheView {
            tokens: source,
            identity: CacheIdentity::new(1),
            minimum_match_tokens: minimum,
        };
        discover(target, window, cache, &mut stopper)
            .expect("discovery")
            .ranges
            .into_iter()
            .map(|r| (r.source_start, r.destination_start, r.token_count))
            .collect()
    }

    /// Brute force: every match that cannot extend left or right inside the
    /// window, at least `minimum` long.
    fn oracle(source: &[u32], target: &[u32], window: Range<usize>, minimum: usize) -> Found {
        let mut found = BTreeSet::new();
        for s in 0..source.len() {
            for t in window.clone() {
                if s > 0 && t > window.start && source[s - 1] == target[t - 1] {
                    continue;
                }
                let mut n = 0;
                while s + n < source.len() && t + n < window.end && source[s + n] == target[t + n] {
                    n += 1;
                }
                if n >= minimum {
                    found.insert((s as u32, t as u32, n as u32));
                }
            }
        }
        found
    }

    #[test]
    fn finds_every_maximal_match_inside_the_window() {
        let mut state = 0x9e37_79b9u64;
        let mut next = || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((state >> 33) % 4) as u32
        };
        for case in 0..200usize {
            let source: Vec<u32> = (0..1 + case % 9).map(|_| next()).collect();
            let target: Vec<u32> = (0..1 + (case / 9) % 9).map(|_| next()).collect();
            let start = case % 3 % (target.len() + 1);
            let end = target.len() - (case / 3 % 2).min(target.len() - start);
            let minimum = 1 + (case % 3) as u32;
            assert_eq!(
                discover_in(&source, &target, start..end, minimum),
                oracle(&source, &target, start..end, minimum as usize),
                "case {case}"
            );
        }
    }

    #[test]
    fn matches_crossing_the_window_are_clipped_not_dropped() {
        // The whole context matches; the window keeps positions 2..6.
        let tokens = [1, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(
            discover_in(&tokens, &tokens, 2..6, 3),
            BTreeSet::from([(2, 2, 4)])
        );
    }
}
