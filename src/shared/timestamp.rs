//! Batched timestamp optimization.
//!
//! Reduces the number of system calls when timestamps are requested at a very
//! high rate:
//! - The real clock is read once every [`UPDATE_INTERVAL`] calls.
//! - Intermediate calls reuse the cached value.
//! - In hot paths this saves roughly 90-100 ns per call.
//!
//! The trade-off is precision: a cached timestamp can lag by up to
//! [`UPDATE_INTERVAL`] calls. Use [`get_precise_timestamp`] when the exact
//! wall-clock time matters.
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Cached timestamp in nanoseconds since the Unix epoch.
static TIMESTAMP_CACHE: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// Number of calls made since the cache was last refreshed.
    static UPDATE_COUNTER: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Number of calls between real clock reads.
const UPDATE_INTERVAL: u32 = 100;

/// Returns a cached timestamp in nanoseconds since the Unix epoch.
///
/// # Performance
/// - One system call per [`UPDATE_INTERVAL`] calls.
/// - All other calls perform an atomic load (~5-10 ns).
///
/// # Accuracy
/// The returned value can lag by at most [`UPDATE_INTERVAL`] calls. For matching
/// engines, relative ordering matters more than absolute precision; use
/// [`get_precise_timestamp`] when exact time is required.
#[inline]
#[must_use]
pub fn get_fast_timestamp() -> u64 {
    UPDATE_COUNTER.with(|counter| {
        let count = counter.get();
        // Refresh on the first call so callers never observe a zero timestamp.
        if count == 0 || count >= UPDATE_INTERVAL {
            let precise = get_precise_timestamp();
            // `fetch_max` keeps the process-wide cache monotonic even if two
            // threads read CLOCK_REALTIME in reverse order.
            let previous = TIMESTAMP_CACHE.fetch_max(precise, Ordering::Relaxed);
            let new_ts = previous.max(precise);
            counter.set(1);
            new_ts
        } else {
            counter.set(count + 1);
            TIMESTAMP_CACHE.load(Ordering::Relaxed)
        }
    })
}

/// Returns the current time in nanoseconds since the Unix epoch, without
/// caching.
#[inline]
#[must_use]
pub fn get_precise_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

/// Forces the cached timestamp to be refreshed.
///
/// Useful when callers need to synchronize the cache with wall-clock time.
pub fn force_update_timestamp() {
    let precise = get_precise_timestamp();
    TIMESTAMP_CACHE.fetch_max(precise, Ordering::Relaxed);

    UPDATE_COUNTER.with(|counter| {
        counter.set(0);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_fast_timestamp_increments() {
        let ts1 = get_fast_timestamp();
        thread::sleep(Duration::from_millis(1));
        force_update_timestamp();
        let ts2 = get_fast_timestamp();

        assert!(ts2 > ts1, "timestamp should increase");
    }

    #[test]
    fn test_first_call_is_not_zero() {
        // A process-wide cache may already be warm, but the very first call in
        // a fresh process must still return a real timestamp rather than zero.
        assert_ne!(get_fast_timestamp(), 0, "first call must read the clock");
    }

    #[test]
    #[ignore = "global timestamp cache is shared across parallel tests and coverage instrumentation"]
    fn test_cache_usage() {
        force_update_timestamp();

        // The next 100 calls should mostly reuse the cached value.
        let ts1 = get_fast_timestamp();
        let mut same_count = 0;

        for _ in 1..50 {
            let ts = get_fast_timestamp();
            if ts == ts1 {
                same_count += 1;
            }
        }

        assert!(same_count > 40, "should use the cache most of the time");
    }

    #[test]
    fn test_precise_timestamp_always_updates() {
        let ts1 = get_precise_timestamp();
        thread::sleep(Duration::from_micros(100));
        let ts2 = get_precise_timestamp();

        assert!(ts2 > ts1, "precise timestamps should always be fresh");
    }

    #[test]
    fn test_concurrent_access() {
        let handles: Vec<_> = (0..4)
            .map(|_| {
                thread::spawn(|| {
                    let mut timestamps = Vec::new();
                    for _ in 0..1000 {
                        timestamps.push(get_fast_timestamp());
                    }
                    timestamps
                })
            })
            .collect();

        for handle in handles {
            let timestamps = handle.join().expect("timestamp thread panicked");
            for pair in timestamps.windows(2) {
                assert!(
                    pair[1] >= pair[0],
                    "timestamps should be monotonic, got {} then {}",
                    pair[0],
                    pair[1]
                );
            }
        }
    }

    #[test]
    #[ignore = "performance test that can be flaky under system load"]
    fn test_performance_comparison() {
        use std::time::Instant;

        let start = Instant::now();
        for _ in 0..10_000 {
            let _ = get_precise_timestamp();
        }
        let precise_duration = start.elapsed();

        force_update_timestamp();

        let start = Instant::now();
        for _ in 0..10_000 {
            let _ = get_fast_timestamp();
        }
        let fast_duration = start.elapsed();

        println!(
            "precise: {precise_duration:?}, fast: {fast_duration:?}, speedup: {:.2}x",
            precise_duration.as_nanos() as f64 / fast_duration.as_nanos() as f64
        );

        // The speedup varies with system load; keep a conservative bound.
        let speedup = precise_duration.as_nanos() as f64 / fast_duration.as_nanos() as f64;
        assert!(
            speedup >= 1.5,
            "fast timestamp should be at least 1.5x faster, got {speedup:.2}x"
        );
    }
}
