//! Lightweight runtime counters for trading operation.
//!
//! These are deliberately plain counters for tests and operator probes; a
//! Prometheus/OTLP exporter can wrap them later without changing the runtime.

/// Fixed-bucket latency histogram (nanoseconds).
pub const LATENCY_BUCKETS_NS: [u64; 12] = [
    1_000, 2_000, 5_000, 10_000, 20_000, 50_000, 100_000, 200_000, 500_000, 1_000_000, 2_000_000,
    5_000_000,
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LatencyHistogram {
    buckets: [u64; 12],
    sum_ns: u64,
    count: u64,
}

impl LatencyHistogram {
    pub fn observe(&mut self, ns: u64) {
        self.count += 1;
        self.sum_ns = self.sum_ns.saturating_add(ns);
        for (index, boundary) in LATENCY_BUCKETS_NS.iter().enumerate() {
            if ns <= *boundary {
                self.buckets[index] += 1;
                return;
            }
        }
        // Values above the last finite bucket are counted by the +Inf bucket,
        // which is derived from `count` during rendering.
    }

    #[must_use]
    pub fn count(&self) -> u64 {
        self.count
    }

    #[must_use]
    pub fn sum_ns(&self) -> u64 {
        self.sum_ns
    }

    fn render(&self, name: &str, out: &mut String) {
        out.push_str("# TYPE ");
        out.push_str(name);
        out.push_str(" histogram\n");
        for (index, boundary) in LATENCY_BUCKETS_NS.iter().enumerate() {
            out.push_str(name);
            out.push_str("_bucket{le=\"");
            out.push_str(&format!("{:.6}", *boundary as f64 / 1e9));
            out.push_str("\"} ");
            out.push_str(&self.buckets[index].to_string());
            out.push('\n');
        }
        out.push_str(name);
        out.push_str("_bucket{le=\"+Inf\"} ");
        out.push_str(&self.count.to_string());
        out.push('\n');
        out.push_str(name);
        out.push_str("_sum ");
        out.push_str(&format!("{:.9}", self.sum_ns as f64 / 1e9));
        out.push('\n');
        out.push_str(name);
        out.push_str("_count ");
        out.push_str(&self.count.to_string());
        out.push('\n');
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TradingMetrics {
    pub gateway_events: u64,
    pub commands_executed: u64,
    pub private_events_sent: u64,
    pub public_events_sent: u64,
    pub repair_events_sent: u64,
    pub idle_sessions_expired: u64,
    pub replay_events_sent: u64,
    pub active_sessions: u64,
    pub closed_sessions: u64,
    pub combo_queue_orders: u64,
    pub fanout_errors: u64,
    pub tick_latency: LatencyHistogram,
}

impl TradingMetrics {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Renders counters in Prometheus text exposition format.
    #[must_use]
    pub fn to_prometheus(&self) -> String {
        let mut out = String::with_capacity(768);
        for (name, value) in [
            ("trading_gateway_events_total", self.gateway_events),
            ("trading_commands_total", self.commands_executed),
            ("trading_private_events_total", self.private_events_sent),
            ("trading_public_events_total", self.public_events_sent),
            ("trading_repair_events_total", self.repair_events_sent),
            ("trading_idle_sessions_total", self.idle_sessions_expired),
            ("trading_replay_events_total", self.replay_events_sent),
        ] {
            out.push_str("# TYPE ");
            out.push_str(name);
            out.push_str(" counter\n");
            out.push_str(name);
            out.push(' ');
            out.push_str(&value.to_string());
            out.push('\n');
        }
        for (name, value) in [
            ("trading_active_sessions", self.active_sessions),
            ("trading_closed_sessions", self.closed_sessions),
            ("trading_combo_queue_orders", self.combo_queue_orders),
            ("trading_fanout_errors_total", self.fanout_errors),
        ] {
            out.push_str("# TYPE ");
            out.push_str(name);
            out.push_str(" gauge\n");
            out.push_str(name);
            out.push(' ');
            out.push_str(&value.to_string());
            out.push('\n');
        }
        self.tick_latency
            .render("trading_tick_latency_seconds", &mut out);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_start_at_zero() {
        let metrics = TradingMetrics::new();
        assert_eq!(metrics, TradingMetrics::default());
    }

    #[test]
    fn renders_prometheus_counters() {
        let metrics = TradingMetrics {
            gateway_events: 3,
            commands_executed: 4,
            private_events_sent: 5,
            public_events_sent: 6,
            repair_events_sent: 7,
            idle_sessions_expired: 8,
            replay_events_sent: 9,
            active_sessions: 10,
            closed_sessions: 11,
            combo_queue_orders: 12,
            fanout_errors: 13,
            tick_latency: LatencyHistogram::default(),
        };
        let text = metrics.to_prometheus();
        assert!(text.contains("trading_gateway_events_total 3\n"));
        assert!(text.contains("trading_commands_total 4\n"));
        assert!(text.contains("trading_public_events_total 6\n"));
        assert!(text.contains("trading_active_sessions 10\n"));
        assert!(text.contains("trading_combo_queue_orders 12\n"));
        assert!(text.contains("trading_fanout_errors_total 13\n"));
    }

    #[test]
    fn latency_histogram_buckets() {
        let mut histogram = LatencyHistogram::default();
        for ns in [500, 1_500, 20_000, 6_000_000] {
            histogram.observe(ns);
        }
        assert_eq!(histogram.count(), 4);
        assert_eq!(histogram.sum_ns(), 6_022_000);
        let mut text = String::new();
        histogram.render("test_latency_seconds", &mut text);
        assert!(text.contains("test_latency_seconds_count 4\n"));
        assert!(text.contains("test_latency_seconds_bucket{le=\"+Inf\"} 4\n"));
    }
}
