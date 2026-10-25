//! Bounded exact maximal-match discovery over one logical cache source.

use std::mem::size_of;

use crate::cache_view::CacheView;
use crate::candidate::{CandidateRange, MAX_CANDIDATES};
use crate::error::AdmissionDeniedReason;
use crate::timing::{CheckOutcome, DeadlineClock};

/// Each input token slice is capped at 1,048,576 entries to bound index work
/// and memory in this Rust planner. This is not a native execution limit.
pub(crate) const MAX_DISCOVERY_TOKENS: usize = 1 << 20;
/// Modeled peak discovery payload; borrowed token slices are not included.
pub(crate) const MAX_DISCOVERY_BYTES: u64 = 32 * 1024 * 1024;

const HASH_BASE: u64 = 257;

/// Whether discovery enumerated all qualifying maximal matches or stopped at
/// the candidate capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryStatus {
    /// All qualifying maximal matches under the declared rule were considered.
    Complete,
    /// More qualifying matches existed after the candidate capacity was filled.
    CandidateLimitReached,
}

#[derive(Debug, Clone, Copy)]
struct Seed {
    hash: u64,
    source: u32,
}

#[derive(Debug)]
pub(crate) enum DiscoveryError {
    Cancelled,
    BudgetExhausted,
    AdmissionDenied(AdmissionDeniedReason),
    Invariant(&'static str),
}

pub(crate) struct DiscoveryOutcome {
    pub ranges: Vec<CandidateRange>,
    pub status: DiscoveryStatus,
}

fn poll(clock: &mut DeadlineClock<'_>) -> Result<(), DiscoveryError> {
    match clock.check() {
        CheckOutcome::Continue => Ok(()),
        CheckOutcome::DeadlineReached => Err(DiscoveryError::BudgetExhausted),
        CheckOutcome::Cancelled => Err(DiscoveryError::Cancelled),
    }
}

fn seed_key(seed: &Seed) -> (u64, u32) {
    (seed.hash, seed.source)
}

/// Accumulate an attempted modeled component. On overflow the attempted total
/// saturates at `u64::MAX`; on failure the caller reports the attempted subtotal
/// that exceeded the limit, not the previous under-limit value.
fn seed_fits(buffer: &mut u64, bytes: u64) -> bool {
    let attempted = buffer.saturating_add(bytes);
    *buffer = attempted;
    attempted <= MAX_DISCOVERY_BYTES
}

fn window_hash(
    tokens: &[u32],
    start: usize,
    minimum: usize,
    clock: &mut DeadlineClock<'_>,
) -> Result<u64, DiscoveryError> {
    let mut hash = 0u64;
    for token in &tokens[start..start + minimum] {
        poll(clock)?;
        hash = hash.wrapping_mul(HASH_BASE).wrapping_add(*token as u64 + 1);
    }
    Ok(hash)
}

fn update_hash(hash: u64, outgoing: u32, incoming: u32, power: u64) -> u64 {
    hash.wrapping_mul(HASH_BASE)
        .wrapping_add(incoming as u64 + 1)
        .wrapping_sub((outgoing as u64 + 1).wrapping_mul(power))
}

/// Sort seeds by `(hash, source)`. The algorithm is deterministic and checks
/// cancellation/deadline during partitioning rather than leaving a million-row
/// sort uninterruptible.
fn sort_seeds(seeds: &mut [Seed], clock: &mut DeadlineClock<'_>) -> Result<(), DiscoveryError> {
    fn insertion(slice: &mut [Seed], clock: &mut DeadlineClock<'_>) -> Result<(), DiscoveryError> {
        for i in 1..slice.len() {
            let mut j = i;
            while j > 0 && seed_key(&slice[j - 1]) > seed_key(&slice[j]) {
                slice.swap(j - 1, j);
                j -= 1;
                poll(clock)?;
            }
            poll(clock)?;
        }
        Ok(())
    }

    fn quicksort(
        mut slice: &mut [Seed],
        clock: &mut DeadlineClock<'_>,
    ) -> Result<(), DiscoveryError> {
        const SMALL: usize = 24;
        loop {
            if slice.len() <= SMALL {
                return insertion(slice, clock);
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
                poll(clock)?;
            }
            // Recurse into the smaller partition and loop over the larger one.
            // Depth is therefore O(log N), independent of ordering and deadline.
            if less <= slice.len() - greater {
                quicksort(&mut slice[..less], clock)?;
                slice = &mut slice[greater..];
            } else {
                quicksort(&mut slice[greater..], clock)?;
                slice = &mut slice[..less];
            }
        }
    }

    quicksort(seeds, clock)
}

fn exact_prefix(
    source: &[u32],
    source_start: usize,
    target: &[u32],
    target_start: usize,
    minimum: usize,
    clock: &mut DeadlineClock<'_>,
) -> Result<bool, DiscoveryError> {
    for offset in 0..minimum {
        poll(clock)?;
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
    clock: &mut DeadlineClock<'_>,
) -> Result<(usize, usize, usize), DiscoveryError> {
    let mut left = 0usize;
    while source_start > left && target_start > left {
        poll(clock)?;
        if source[source_start - left - 1] != target[target_start - left - 1] {
            break;
        }
        left += 1;
    }
    let mut right = minimum;
    while source_start + right < source.len() && target_start + right < target.len() {
        poll(clock)?;
        if source[source_start + right] != target[target_start + right] {
            break;
        }
        right += 1;
    }
    Ok((source_start - left, target_start - left, left + right))
}

/// Discover every maximal exact match at or above `cache.minimum_match_tokens`.
///
/// Seeds exist at every logical source and target position; there is no native
/// parity/8 stride. Hash equality is only a filter: every seed pair is verified
/// token by token. A per-diagonal coverage end suppresses duplicate discoveries
/// from windows inside the same maximal run. Candidate capacity is the only
/// intentional early stop and is reported in [`DiscoveryStatus`].
pub(crate) fn discover(
    context: &[u32],
    cache: CacheView<'_>,
    clock: &mut DeadlineClock<'_>,
) -> Result<DiscoveryOutcome, DiscoveryError> {
    let minimum = cache.minimum_match_tokens as usize;
    let source = cache.tokens;
    if minimum == 0
        || source.is_empty()
        || context.is_empty()
        || minimum > source.len()
        || minimum > context.len()
    {
        return Ok(DiscoveryOutcome {
            ranges: Vec::new(),
            status: DiscoveryStatus::Complete,
        });
    }

    let source_windows = source.len() - minimum + 1;
    let covered_len = source
        .len()
        .checked_add(context.len())
        .and_then(|len| len.checked_add(1))
        .ok_or(DiscoveryError::Invariant(
            "discovery coordinate length overflows",
        ))?;
    let mut modeled = 0u64;
    if !seed_fits(
        &mut modeled,
        (source_windows as u64)
            .checked_mul(size_of::<Seed>() as u64)
            .ok_or(DiscoveryError::Invariant("discovery seed bytes overflow"))?,
    ) || !seed_fits(
        &mut modeled,
        (covered_len as u64)
            .checked_mul(size_of::<u32>() as u64)
            .ok_or(DiscoveryError::Invariant(
                "discovery coverage bytes overflow",
            ))?,
    ) || !seed_fits(
        &mut modeled,
        (MAX_CANDIDATES as u64)
            .checked_mul(size_of::<CandidateRange>() as u64)
            .ok_or(DiscoveryError::Invariant(
                "discovery candidate bytes overflow",
            ))?,
    ) {
        return Err(DiscoveryError::AdmissionDenied(
            AdmissionDeniedReason::DiscoveryBuffer {
                bytes: modeled,
                max: MAX_DISCOVERY_BYTES,
            },
        ));
    }

    let mut seeds = Vec::new();
    seeds
        .try_reserve_exact(source_windows)
        .map_err(|_| DiscoveryError::AdmissionDenied(AdmissionDeniedReason::Allocation))?;
    let mut covered = Vec::new();
    covered
        .try_reserve_exact(covered_len)
        .map_err(|_| DiscoveryError::AdmissionDenied(AdmissionDeniedReason::Allocation))?;
    let mut ranges = Vec::new();
    ranges
        .try_reserve_exact(MAX_CANDIDATES)
        .map_err(|_| DiscoveryError::AdmissionDenied(AdmissionDeniedReason::Allocation))?;

    let mut power = 1u64;
    for _ in 0..minimum {
        poll(clock)?;
        power = power.wrapping_mul(HASH_BASE);
    }
    let mut hash = window_hash(source, 0, minimum, clock)?;
    let mut i = 0usize;
    while i < source_windows {
        poll(clock)?;
        if i > 0 {
            hash = update_hash(hash, source[i - 1], source[i + minimum - 1], power);
        }
        seeds.push(Seed {
            hash,
            source: i as u32,
        });
        i += 1;
    }
    sort_seeds(&mut seeds, clock)?;

    for _ in 0..covered_len {
        poll(clock)?;
        covered.push(0u32);
    }

    let mut target_hash = window_hash(context, 0, minimum, clock)?;
    let mut destination = 0usize;
    let target_windows = context.len() - minimum + 1;
    while destination < target_windows {
        poll(clock)?;
        if destination > 0 {
            target_hash = update_hash(
                target_hash,
                context[destination - 1],
                context[destination + minimum - 1],
                power,
            );
        }
        let mut lo = 0usize;
        let mut hi = seeds.len();
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if seeds[mid].hash < target_hash {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let mut pair = lo;
        while pair < seeds.len() && seeds[pair].hash == target_hash {
            poll(clock)?;
            let source_start = seeds[pair].source as usize;
            let diagonal = source_start + context.len() - destination;
            let covered_end = covered[diagonal];
            if covered_end < (destination + minimum) as u32
                && exact_prefix(source, source_start, context, destination, minimum, clock)?
            {
                let (start, begin, count) =
                    extend_match(source, source_start, context, destination, minimum, clock)?;
                covered[diagonal] = (begin + count) as u32;
                if ranges.len() == MAX_CANDIDATES {
                    return match clock.check() {
                        CheckOutcome::Cancelled => Err(DiscoveryError::Cancelled),
                        CheckOutcome::DeadlineReached => Err(DiscoveryError::BudgetExhausted),
                        CheckOutcome::Continue => Ok(DiscoveryOutcome {
                            ranges,
                            status: DiscoveryStatus::CandidateLimitReached,
                        }),
                    };
                }
                ranges.push(CandidateRange {
                    source_start: start as u32,
                    destination_start: begin as u32,
                    token_count: count as u32,
                });
            }
            pair += 1;
        }
        destination += 1;
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
    use crate::timing::DeadlineClock;
    use std::collections::BTreeSet;
    use std::time::{Duration, Instant};

    fn clock(base: Instant) -> impl FnMut() -> Instant {
        move || base
    }

    fn discover_ranges(source: &[u32], target: &[u32], minimum: u32) -> Vec<CandidateRange> {
        let base = Instant::now();
        let mut cancel = || false;
        let mut clock = DeadlineClock::with_clock(
            base,
            base + Duration::from_secs(5),
            clock(base),
            &mut cancel,
        );
        let cache = CacheView {
            tokens: source,
            identity: CacheIdentity::new(1),
            minimum_match_tokens: minimum,
        };
        discover(target, cache, &mut clock)
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
    fn modeled_admission_reports_the_attempted_overlimit_total() {
        let mut at_limit = MAX_DISCOVERY_BYTES - 1;
        assert!(seed_fits(&mut at_limit, 1));
        assert_eq!(at_limit, MAX_DISCOVERY_BYTES);

        let mut over = MAX_DISCOVERY_BYTES;
        assert!(!seed_fits(&mut over, 1));
        assert_eq!(over, MAX_DISCOVERY_BYTES + 1);

        let mut overflow = u64::MAX;
        assert!(!seed_fits(&mut overflow, 1));
        assert_eq!(overflow, u64::MAX);
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
            let actual: BTreeSet<_> = discover_ranges(&source, &target, minimum)
                .into_iter()
                .map(|r| (r.source_start, r.destination_start, r.token_count))
                .collect();
            let expected = maximal_oracle(&source, &target, minimum as usize);
            assert_eq!(actual, expected, "case {case}");
        }
    }

    #[test]
    fn seed_sort_polls_cancellation() {
        let base = Instant::now();
        let mut seeds: Vec<Seed> = (0..5_000u32)
            .rev()
            .map(|source| Seed {
                hash: source as u64,
                source,
            })
            .collect();
        let mut cancel = || true;
        let mut clock = DeadlineClock::with_clock(
            base,
            base + Duration::from_secs(5),
            clock(base),
            &mut cancel,
        );
        assert!(matches!(
            sort_seeds(&mut seeds, &mut clock),
            Err(DiscoveryError::Cancelled)
        ));
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
