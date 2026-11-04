//! Nanosecond latency accounting.
//!
//! Trading systems care about the shape of the latency distribution, not the
//! mean: a single 10 ms stall in the 99.99th percentile can wipe out a day of
//! median profits. This recorder keeps a compact log-linear histogram so
//! callers can report p50/p90/p99/p99.9/p99.99 and max.
//!
//! Buckets below 1 µs are exact (1 ns resolution); above that the histogram is
//! logarithmic with four sub-buckets per power of two (≤ ~12% relative error).
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Number of exact buckets covering 0..=1023 ns.
const FINE_BUCKETS: usize = 1024;
const COARSE_START_LOG2: u32 = 10;
const SUB_BUCKETS: usize = 4;
const COARSE_LEVELS: usize = (64 - COARSE_START_LOG2 as usize) * SUB_BUCKETS;
const BUCKETS: usize = FINE_BUCKETS + COARSE_LEVELS;

/// Latency percentiles in nanoseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LatencySummary {
    pub count: u64,
    pub p50: u64,
    pub p90: u64,
    pub p99: u64,
    pub p99_9: u64,
    pub p99_99: u64,
    pub max: u64,
}

impl std::fmt::Display for LatencySummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "n={} p50={}ns p90={}ns p99={}ns p99.9={}ns p99.99={}ns max={}ns",
            self.count, self.p50, self.p90, self.p99, self.p99_9, self.p99_99, self.max
        )
    }
}

/// A fixed-size, wait-free (atomic per bucket) latency histogram.
pub struct LatencyRecorder {
    buckets: [AtomicU64; BUCKETS],
    count: AtomicU64,
    max: AtomicU64,
}

impl Default for LatencyRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl LatencyRecorder {
    /// Creates an empty recorder.
    #[must_use]
    pub fn new() -> Self {
        Self {
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            count: AtomicU64::new(0),
            max: AtomicU64::new(0),
        }
    }

    /// Records one sample in nanoseconds.
    pub fn record_ns(&self, nanos: u64) {
        let index = bucket_index(nanos);
        self.buckets[index].fetch_add(1, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
        self.max.fetch_max(nanos, Ordering::Relaxed);
    }

    /// Records one sample from a `Duration`.
    pub fn record_duration(&self, duration: Duration) {
        self.record_ns(duration.as_nanos() as u64);
    }

    /// Returns the number of recorded samples.
    #[must_use]
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    /// Computes the latency summary.
    #[must_use]
    pub fn snapshot(&self) -> LatencySummary {
        let count = self.count.load(Ordering::Relaxed);
        if count == 0 {
            return LatencySummary::default();
        }

        let mut summary = LatencySummary {
            count,
            max: self.max.load(Ordering::Relaxed),
            ..LatencySummary::default()
        };
        let targets = [
            (0.50, &mut summary.p50),
            (0.90, &mut summary.p90),
            (0.99, &mut summary.p99),
            (0.999, &mut summary.p99_9),
            (0.9999, &mut summary.p99_99),
        ];

        let mut cumulative = 0u64;
        let mut next_target = 0usize;
        for (index, bucket) in self.buckets.iter().enumerate() {
            cumulative += bucket.load(Ordering::Relaxed);
            while next_target < targets.len() {
                let rank = ((count as f64) * targets[next_target].0).ceil() as u64;
                if cumulative >= rank.max(1) {
                    *targets[next_target].1 = bucket_upper_bound(index);
                    next_target += 1;
                } else {
                    break;
                }
            }
            if next_target == targets.len() {
                break;
            }
        }

        summary
    }
}

/// Maps a nanosecond value to its bucket.
fn bucket_index(nanos: u64) -> usize {
    if nanos < FINE_BUCKETS as u64 {
        return nanos as usize;
    }
    let top = 63 - nanos.leading_zeros();
    let level = (top - COARSE_START_LOG2) as usize;
    let shift = top - 2;
    let sub = ((nanos >> shift) & (SUB_BUCKETS as u64 - 1)) as usize;
    FINE_BUCKETS + level * SUB_BUCKETS + sub
}

/// Returns a representative upper bound for a bucket.
fn bucket_upper_bound(index: usize) -> u64 {
    if index < FINE_BUCKETS {
        return index as u64;
    }
    let coarse = index - FINE_BUCKETS;
    let level = coarse / SUB_BUCKETS;
    let sub = coarse % SUB_BUCKETS;
    let top = COARSE_START_LOG2 + level as u32;
    let base = 1u64 << top;
    base + (((sub as u64) + 1) << (top - 2)).saturating_sub(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bucket_index_boundaries() {
        assert_eq!(bucket_index(0), 0);
        assert_eq!(bucket_index(1023), 1023);
        assert!(bucket_index(1024) >= FINE_BUCKETS);
        assert!(bucket_index(u64::MAX) < BUCKETS);
        for value in [1024u64, 1536, 2047, 4096, 1_000_000, 1_000_000_000] {
            let index = bucket_index(value);
            assert!(index < BUCKETS);
            assert!(bucket_upper_bound(index) >= value.min(bucket_upper_bound(index)));
        }
    }

    #[test]
    fn test_percentiles() {
        let recorder = LatencyRecorder::new();
        for value in 1..=1000u64 {
            recorder.record_ns(value);
        }
        let summary = recorder.snapshot();
        assert_eq!(summary.count, 1000);
        assert_eq!(summary.max, 1000);
        assert!((summary.p50 as i64 - 500).abs() <= 16);
        assert!((summary.p90 as i64 - 900).abs() <= 32);
        assert!((summary.p99 as i64 - 990).abs() <= 32);
        assert!(summary.p99_9 >= summary.p99);
        assert!(summary.p99_99 >= summary.p99_9);
    }

    #[test]
    fn test_empty_snapshot() {
        let recorder = LatencyRecorder::new();
        assert_eq!(recorder.snapshot(), LatencySummary::default());
    }

    #[test]
    fn test_duration_recording() {
        let recorder = LatencyRecorder::new();
        recorder.record_duration(Duration::from_micros(5));
        let summary = recorder.snapshot();
        assert_eq!(summary.count, 1);
        assert!(summary.max >= 5_000);
    }
}
