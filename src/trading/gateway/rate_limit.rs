//! Per-connection token-bucket rate limiting for gateway frames.

use std::time::Instant;

/// A simple monotonic token bucket.
#[derive(Debug)]
pub struct TokenBucket {
    capacity: f64,
    refill_per_sec: f64,
    tokens: f64,
    last: Instant,
}

impl TokenBucket {
    #[must_use]
    pub fn new(capacity: f64, refill_per_sec: f64) -> Self {
        Self {
            capacity,
            refill_per_sec,
            tokens: capacity,
            last: Instant::now(),
        }
    }

    /// Takes one token, refilling based on elapsed time.
    pub fn try_acquire(&mut self) -> bool {
        self.try_acquire_at(Instant::now())
    }

    fn try_acquire_at(&mut self, now: Instant) -> bool {
        let elapsed = now.duration_since(self.last).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        self.last = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    #[must_use]
    pub fn tokens(&self) -> f64 {
        self.tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn bucket_exhausts_and_refills() {
        let mut bucket = TokenBucket::new(1.0, 10.0);
        assert!(bucket.try_acquire_at(Instant::now()));
        assert!(!bucket.try_acquire_at(Instant::now()));
        // 100ms should add one token at 10/s.
        assert!(bucket.try_acquire_at(Instant::now() + Duration::from_millis(100)));
    }

    #[test]
    fn capacity_is_capped() {
        let mut bucket = TokenBucket::new(2.0, 100.0);
        assert!(bucket.try_acquire_at(Instant::now()));
        assert!(bucket.try_acquire_at(Instant::now()));
        assert!(bucket.try_acquire_at(Instant::now() + Duration::from_secs(1)));
        assert!(bucket.tokens() <= 1.0);
    }
}
