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

const NONE: u32 = u32::MAX;

/// Most equal-hash source windows examined per target window. A longer
/// bucket means highly repetitive input; the rest of it is skipped and
/// discovery reports [`DiscoveryStatus::CandidateLimitReached`]. A long match
/// is still found from any of its windows whose bucket is within the bound,
/// because matches extend left as well as right.
const MAX_BUCKET_PROBES: usize = 256;

/// Probe bound for a target window inside a run of at least
/// [`LONG_RUN`] copies of one token, past the run's start. Such a window's
/// matches mostly continue matches already found from the previous window,
/// so few probes are new.
const MAX_RUN_PROBES: usize = 16;
const LONG_RUN: usize = 64;

/// One input with its runs of equal tokens: for each index, where its run
/// starts and ends. Extension jumps over shared runs in one step, so a long
/// single-token repeat costs O(1) per match instead of O(length).
struct Side<'a> {
    tokens: &'a [u32],
    run_start: Vec<u32>,
    run_end: Vec<u32>,
}

impl<'a> Side<'a> {
    fn new(tokens: &'a [u32]) -> Self {
        let mut run_start = vec![0u32; tokens.len()];
        let mut run_end = vec![tokens.len() as u32; tokens.len()];
        for i in 1..tokens.len() {
            if tokens[i] == tokens[i - 1] {
                run_start[i] = run_start[i - 1];
            } else {
                run_start[i] = i as u32;
            }
        }
        for i in (0..tokens.len().saturating_sub(1)).rev() {
            if tokens[i] == tokens[i + 1] {
                run_end[i] = run_end[i + 1];
            } else {
                run_end[i] = i as u32 + 1;
            }
        }
        Self {
            tokens,
            run_start,
            run_end,
        }
    }
}

/// Extend the verified window at `(s, t)` to a maximal match. Returns
/// `(source_start, target_start, count)`.
fn extend_match(
    source: &Side<'_>,
    s: usize,
    target: &Side<'_>,
    t: usize,
    minimum: usize,
    stopper: &mut Stopper<'_>,
) -> Result<(usize, usize, usize), Stop> {
    let mut left = 0usize;
    while s > left && t > left {
        let (a, b) = (s - left - 1, t - left - 1);
        if source.tokens[a] != target.tokens[b] {
            break;
        }
        stopper.tick()?;
        let run_a = a + 1 - source.run_start[a] as usize;
        let run_b = b + 1 - target.run_start[b] as usize;
        left += run_a.min(run_b);
    }
    let mut right = minimum;
    while s + right < source.tokens.len() && t + right < target.tokens.len() {
        let (a, b) = (s + right, t + right);
        if source.tokens[a] != target.tokens[b] {
            break;
        }
        stopper.tick()?;
        let run_a = source.run_end[a] as usize - a;
        let run_b = target.run_end[b] as usize - b;
        right += run_a.min(run_b);
    }
    Ok((s - left, t - left, left + right))
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
    /// Whether a match of `count` tokens would be dropped by a full heap.
    fn cannot_enter(&self, count: u32) -> bool {
        self.heap.len() == MAX_CANDIDATES
            && self
                .heap
                .peek()
                .is_some_and(|Reverse((weakest, _, _))| count < *weakest)
    }

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
/// Source windows go into a chained hash table in one pass, with chains in
/// ascending source order. Each target window walks its chain; hash equality
/// is only a filter, and every pair is verified token by token. A
/// per-diagonal end skips windows inside a match already found, and once the
/// candidate heap is full a pair that cannot beat its weakest entry is not
/// extended. Destinations are in full-context coordinates.
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
    let windows = source.len() - minimum + 1;
    let mut hashes = Vec::with_capacity(windows);
    let mut hash = window_hash(source, 0, minimum);
    for i in 0..windows {
        stopper.tick()?;
        if i > 0 {
            hash = update_hash(hash, source[i - 1], source[i + minimum - 1], power);
        }
        hashes.push(hash);
    }
    let bits = (2 * windows).next_power_of_two().trailing_zeros();
    let slot = |hash: u64| (hash.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> (64 - bits)) as usize;
    let mut head = vec![NONE; 1 << bits];
    let mut next = vec![NONE; windows];
    for i in (0..windows).rev() {
        let bucket = &mut head[slot(hashes[i])];
        next[i] = *bucket;
        *bucket = i as u32;
    }
    stopper.check()?;

    let source = Side::new(source);
    let target = Side::new(target);
    let (source_len, target_len) = (source.tokens.len(), target.tokens.len());
    // Diagonal `source - destination + target_len` -> end of the last match
    // found on it, in target coordinates.
    let mut covered = vec![0u32; source_len + target_len + 1];
    let mut target_hash = window_hash(target.tokens, 0, minimum);
    for destination in 0..=target_len - minimum {
        stopper.tick()?;
        if destination > 0 {
            target_hash = update_hash(
                target_hash,
                target.tokens[destination - 1],
                target.tokens[destination + minimum - 1],
                power,
            );
        }
        let last = destination + minimum - 1;
        let run_start = target.run_start[last] as usize;
        let in_run =
            run_start < destination && target.run_end[last] as usize - run_start >= LONG_RUN;
        let max_probes = if in_run {
            MAX_RUN_PROBES
        } else {
            MAX_BUCKET_PROBES
        };
        let mut probes = 0;
        let mut cursor = head[slot(target_hash)];
        while cursor != NONE {
            let source_start = cursor as usize;
            cursor = next[source_start];
            if hashes[source_start] != target_hash {
                continue;
            }
            if probes == max_probes {
                longest.dropped = true;
                break;
            }
            probes += 1;
            stopper.tick()?;
            let diagonal = source_start + target_len - destination;
            if covered[diagonal] >= (destination + minimum) as u32
                || source.tokens[source_start..source_start + minimum]
                    != target.tokens[destination..destination + minimum]
            {
                continue;
            }
            // The longest match that could pass through this window.
            let bound = source_start.min(destination)
                + (source_len - source_start).min(target_len - destination);
            if longest.cannot_enter(bound as u32) {
                longest.dropped = true;
                continue;
            }
            let (start, begin, count) = extend_match(
                &source,
                source_start,
                &target,
                destination,
                minimum,
                stopper,
            )?;
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
            (state >> 33) as u32
        };
        for case in 0..600usize {
            // Two- and three-token alphabets produce long runs and repeats.
            let alphabet = 2 + (case % 3) as u32;
            let source: Vec<u32> = (0..1 + case % 29).map(|_| next() % alphabet).collect();
            let target: Vec<u32> = (0..1 + (case / 7) % 31)
                .map(|_| next() % alphabet)
                .collect();
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
    fn a_long_single_token_repeat_is_bounded_and_keeps_the_longest_match() {
        // Every source window shares one hash, so each target window's bucket
        // is the whole source. Probes are capped and runs extend in one step.
        let tokens = vec![7u32; 100_000];
        let mut cancel = || false;
        let mut stopper = Stopper::new(Instant::now() + Duration::from_secs(60), &mut cancel);
        let cache = CacheView {
            tokens: &tokens,
            identity: CacheIdentity::new(1),
            minimum_match_tokens: 4,
        };
        let outcome = discover(&tokens, 0..tokens.len(), cache, &mut stopper).expect("discovery");
        assert_eq!(outcome.status, DiscoveryStatus::CandidateLimitReached);
        assert_eq!(outcome.ranges.len(), MAX_CANDIDATES);
        assert!(outcome.ranges.contains(&CandidateRange {
            source_start: 0,
            destination_start: 0,
            token_count: 100_000,
        }));
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
