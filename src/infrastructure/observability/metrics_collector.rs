//! Live connection tracking used by health checks.
//!
//! Only metrics that are actually updated live here; matching metrics are
//! exported through [`crate::shared::metrics`].
use std::sync::LazyLock;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Tracks the number of currently connected clients.
pub struct MetricsCollector {
    active_connections: AtomicUsize,
}

impl MetricsCollector {
    /// Creates an empty collector.
    #[must_use]
    pub fn new() -> Self {
        Self {
            active_connections: AtomicUsize::new(0),
        }
    }

    /// Records a newly accepted connection.
    pub fn inc_connections(&self) {
        self.active_connections.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a closed connection.
    pub fn dec_connections(&self) {
        self.active_connections.fetch_sub(1, Ordering::Relaxed);
    }

    /// Returns the number of active connections.
    #[must_use]
    pub fn active_connections(&self) -> usize {
        self.active_connections.load(Ordering::Relaxed)
    }
}

impl Default for MetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}

/// Global connection tracker.
pub static GLOBAL_METRICS: LazyLock<MetricsCollector> = LazyLock::new(MetricsCollector::new);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connection_counting() {
        let collector = MetricsCollector::new();

        assert_eq!(collector.active_connections(), 0);
        collector.inc_connections();
        assert_eq!(collector.active_connections(), 1);
        collector.dec_connections();
        assert_eq!(collector.active_connections(), 0);
    }
}
