//! Spike S4 (PR-6 / DR-004) — STATS slice: min / median / p95 / max over a
//! nonempty run of u64 wall-clock milliseconds.
//!
//! This module is a pure, allocation-only summary of an already-collected sample
//! vector. It owns NO timing, NO child spawning, and NO I/O. The runner captures
//! wall-ms per measured iteration (warmup excluded) and hands the slice here; the
//! report layer reads [`Stats`] back.
//!
//! The math is deliberately elementary and overflow-safe so two near-`u64::MAX`
//! samples cannot panic or wrap:
//!   * `min` / `max` are order statistics on a sorted copy;
//!   * `median` is the classic overflow-safe midpoint `lo + (hi - lo) / 2`,
//!     rounded DOWN (floor of the true mean), so an even count never adds two
//!     huge u64s together;
//!   * `p95` is the NEAREST-RANK percentile (ordinal rank = `ceil(0.95 * n)`,
//!     1-indexed), computed exactly with integer division and no floats.
//!
//! There is exactly one error: an empty input. The sample vector is copied before
//! sorting so the caller's slice is never mutated.

use std::fmt;

/// Min / median / p95 / max computed over a nonempty sample run.
///
/// All four fields are in the same units the caller passed in (wall-ms in the
/// runner). `median` is the floor of the arithmetic mean of the two middle
/// elements for an even count; `p95` is the nearest-rank 95th percentile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    /// Smallest sample.
    pub min: u64,
    /// Middle sample (odd count) or floor of the mean of the two middle samples
    /// (even count); overflow-safe.
    pub median: u64,
    /// Nearest-rank 95th percentile.
    pub p95: u64,
    /// Largest sample.
    pub max: u64,
}

/// Error returned by [`compute`]. The only failure mode is an empty input; the
/// runner always supplies >= 1 measured sample, so observing this at runtime is a
/// harness bug, surfaced as a stable machine-readable error rather than a panic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatsError {
    /// [`compute`] was called with a zero-length slice.
    Empty,
}

impl fmt::Display for StatsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StatsError::Empty => f.write_str("stats: empty sample vector"),
        }
    }
}

impl std::error::Error for StatsError {}

/// Compute [`Stats`] over a nonempty slice of u64 milliseconds.
///
/// The slice is copied and sorted (the caller's data is never mutated); `min` and
/// `max` come from the sorted copy's ends; the median uses an overflow-safe
/// midpoint rounded down; `p95` uses the nearest-rank method. Returns
/// [`StatsError::Empty`] for a zero-length input.
pub fn compute(samples: &[u64]) -> Result<Stats, StatsError> {
    let n = samples.len();
    if n == 0 {
        return Err(StatsError::Empty);
    }

    // Copy + sort so percentile/order lookups are O(1) and the caller's slice is
    // untouched. `sort_unstable` is fine: equal u64s are indistinguishable, so
    // there is no stability distinction to preserve.
    let mut sorted: Vec<u64> = samples.to_vec();
    sorted.sort_unstable();

    let min = sorted[0];
    let max = sorted[n - 1];

    // Median: middle element (odd), else floor of the mean of the two middle
    // elements (even). `lo + (hi - lo) / 2` is the overflow-safe midpoint rounded
    // down: `hi - lo` cannot underflow (lo <= hi) and the final add cannot exceed
    // `hi` (<= u64::MAX), so two near-MAX samples never overflow.
    let median = if n % 2 == 1 {
        sorted[n / 2]
    } else {
        let lo = sorted[n / 2 - 1];
        let hi = sorted[n / 2];
        lo + (hi - lo) / 2
    };

    // Nearest-rank p95: ordinal rank r = ceil(0.95 * n) (1-indexed), so the
    // 0-based index is r - 1 = ceil(0.95n) - 1 = n - floor(0.05n) - 1 = n - n/20
    // - 1 (integer floor division; 0.05n = n/20 exactly). This is exact,
    // float-free, and overflow-free (`n / 20` cannot overflow). For n < 20 the
    // index is n - 1 (the maximum); for n = 20 it is 18 (the 19th-smallest); for
    // n = 100 it is 94 (the 95th-smallest).
    let p95 = sorted[n - n / 20 - 1];

    Ok(Stats {
        min,
        median,
        p95,
        max,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_an_error() {
        assert_eq!(compute(&[]), Err(StatsError::Empty));
    }

    #[test]
    fn single_sample_is_all_equal() {
        let s = compute(&[42]).unwrap();
        assert_eq!(
            s,
            Stats {
                min: 42,
                median: 42,
                p95: 42,
                max: 42
            }
        );
    }

    #[test]
    fn odd_count_picks_middle() {
        // sorted: [1, 2, 3] -> median 2; n=3 -> p95 index 2 -> 3 (the max).
        let s = compute(&[3, 1, 2]).unwrap();
        assert_eq!(
            s,
            Stats {
                min: 1,
                median: 2,
                p95: 3,
                max: 3
            }
        );
    }

    #[test]
    fn even_count_midpoint_rounded_down() {
        // sorted: [1, 2, 3, 4] -> floor((2 + 3) / 2) = 2, not 3.
        let s = compute(&[1, 2, 3, 4]).unwrap();
        assert_eq!(
            s,
            Stats {
                min: 1,
                median: 2,
                p95: 4,
                max: 4
            }
        );

        // Explicit floor: (2 + 3) / 2 = 2 (floor of 2.5).
        assert_eq!(compute(&[2, 3]).unwrap().median, 2);
        // Exact midpoint: (2 + 4) / 2 = 3.
        assert_eq!(compute(&[2, 4]).unwrap().median, 3);
        // (1 + 4) / 2 = 2 (floor of 2.5).
        assert_eq!(compute(&[1, 4]).unwrap().median, 2);
    }

    #[test]
    fn duplicates_collapse() {
        let s = compute(&[5, 5, 5, 5]).unwrap();
        assert_eq!(
            s,
            Stats {
                min: 5,
                median: 5,
                p95: 5,
                max: 5
            }
        );
        let s2 = compute(&[7, 7]).unwrap();
        assert_eq!(
            s2,
            Stats {
                min: 7,
                median: 7,
                p95: 7,
                max: 7
            }
        );
    }

    #[test]
    fn p95_nearest_rank_at_various_sizes() {
        // n = 10: rank ceil(9.5) = 10 -> max (index 9).
        let v: Vec<u64> = (1..=10).collect();
        assert_eq!(compute(&v).unwrap().p95, 10);

        // n = 20: rank 19 -> element 19 (index 18); median (10 + 11) / 2 = 10.
        let v: Vec<u64> = (1..=20).collect();
        let s = compute(&v).unwrap();
        assert_eq!(s.p95, 19);
        assert_eq!(s.median, 10);

        // n = 21: rank ceil(19.95) = 20 -> element 20 (index 19).
        let v: Vec<u64> = (1..=21).collect();
        assert_eq!(compute(&v).unwrap().p95, 20);

        // n = 100: rank 95 -> element 95 (index 94); median (50 + 51) / 2 = 50.
        let v: Vec<u64> = (1..=100).collect();
        let s = compute(&v).unwrap();
        assert_eq!(s.p95, 95);
        assert_eq!(s.median, 50);
        assert_eq!(s.min, 1);
        assert_eq!(s.max, 100);
    }

    #[test]
    fn median_overflow_safe_near_u64_max() {
        // Even count, two near-MAX values: a naive `(a + b) / 2` would overflow
        // (a + b = 2 * u64::MAX - 1 > u64::MAX) and panic in debug builds.
        let a = u64::MAX - 1;
        let b = u64::MAX;
        let s = compute(&[a, b]).unwrap();
        // floor(((MAX - 1) + MAX) / 2) = MAX - 1.
        assert_eq!(s.median, u64::MAX - 1);
        assert_eq!(s.min, u64::MAX - 1);
        assert_eq!(s.max, u64::MAX);
        assert_eq!(s.p95, u64::MAX); // n = 2 -> p95 = max

        // Odd count, single MAX.
        let s2 = compute(&[u64::MAX]).unwrap();
        assert_eq!(
            s2,
            Stats {
                min: u64::MAX,
                median: u64::MAX,
                p95: u64::MAX,
                max: u64::MAX
            }
        );

        // Three large, odd -> middle element after sort.
        let s3 = compute(&[u64::MAX, u64::MAX - 2, u64::MAX - 1]).unwrap();
        assert_eq!(s3.median, u64::MAX - 1);
        assert_eq!(s3.min, u64::MAX - 2);
        assert_eq!(s3.max, u64::MAX);
        assert_eq!(s3.p95, u64::MAX); // n = 3 -> p95 index 2 -> max

        // Even count where the two middle values straddle MAX/2: still safe.
        let lo = u64::MAX / 2; // 2^63 - 1
        let hi = u64::MAX / 2 + 1; // 2^63
        assert_eq!(compute(&[lo, hi]).unwrap().median, lo); // floor((lo + hi) / 2) = lo
    }

    #[test]
    fn does_not_mutate_caller_slice() {
        let input = vec![3, 1, 2];
        let _ = compute(&input).unwrap();
        assert_eq!(input, vec![3, 1, 2]); // original order preserved
    }
}
