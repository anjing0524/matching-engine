//! Lightweight runtime counters for exchange operation.
//!
//! These are deliberately plain counters for tests and operator probes; a
//! Prometheus/OTLP exporter can wrap them later without changing the runtime.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExchangeMetrics {
    pub gateway_events: u64,
    pub commands_executed: u64,
    pub private_events_sent: u64,
    pub public_events_sent: u64,
    pub repair_events_sent: u64,
    pub idle_sessions_expired: u64,
    pub replay_events_sent: u64,
}

impl ExchangeMetrics {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_start_at_zero() {
        let metrics = ExchangeMetrics::new();
        assert_eq!(metrics, ExchangeMetrics::default());
    }
}
