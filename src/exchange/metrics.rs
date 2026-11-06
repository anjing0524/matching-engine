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

    /// Renders counters in Prometheus text exposition format.
    #[must_use]
    pub fn to_prometheus(&self) -> String {
        let mut out = String::with_capacity(768);
        for (name, value) in [
            ("exchange_gateway_events_total", self.gateway_events),
            ("exchange_commands_total", self.commands_executed),
            ("exchange_private_events_total", self.private_events_sent),
            ("exchange_public_events_total", self.public_events_sent),
            ("exchange_repair_events_total", self.repair_events_sent),
            ("exchange_idle_sessions_total", self.idle_sessions_expired),
            ("exchange_replay_events_total", self.replay_events_sent),
        ] {
            out.push_str("# TYPE ");
            out.push_str(name);
            out.push_str(" counter\n");
            out.push_str(name);
            out.push(' ');
            out.push_str(&value.to_string());
            out.push('\n');
        }
        out
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

    #[test]
    fn renders_prometheus_counters() {
        let metrics = ExchangeMetrics {
            gateway_events: 3,
            commands_executed: 4,
            private_events_sent: 5,
            public_events_sent: 6,
            repair_events_sent: 7,
            idle_sessions_expired: 8,
            replay_events_sent: 9,
        };
        let text = metrics.to_prometheus();
        assert!(text.contains("exchange_gateway_events_total 3\n"));
        assert!(text.contains("exchange_commands_total 4\n"));
        assert!(text.contains("exchange_public_events_total 6\n"));
    }
}
