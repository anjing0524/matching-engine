//! Prometheus metrics.
//!
//! Every metric here is recorded by the matching service or the network layer;
//! metrics that were never updated were removed rather than exported as zeros.
//!
//! # Panics
//! [`METRICS`] panics if registration fails, which indicates a programming
//! error (duplicate registration).
use prometheus::{
    CounterVec, Encoder, HistogramVec, IntCounter, IntGauge, TextEncoder, register_counter_vec,
    register_histogram_vec, register_int_counter, register_int_gauge,
};
use std::sync::LazyLock;

/// Error returned when exporting metrics.
#[derive(Debug, thiserror::Error)]
pub enum MetricsError {
    #[error("failed to encode metrics: {0}")]
    Encode(#[from] prometheus::Error),

    #[error("metrics output is not valid UTF-8: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
}

/// Global metrics instance.
///
/// # Panics
/// Initialization panics if Prometheus registration fails, which indicates
/// duplicate metric registration (a programming error).
pub static METRICS: LazyLock<Metrics> = LazyLock::new(|| {
    Metrics::new().expect("failed to register Prometheus metrics: duplicate registration?")
});

/// Nanosecond buckets: exact enough around the 100 ns–10 µs range that matters
/// for an in-process matching engine, while keeping cardinality bounded.
fn latency_buckets() -> Vec<f64> {
    vec![
        50.0,
        100.0,
        200.0,
        500.0,
        1_000.0,
        2_000.0,
        5_000.0,
        10_000.0,
        20_000.0,
        50_000.0,
        100_000.0,
        250_000.0,
        500_000.0,
        1_000_000.0,
        5_000_000.0,
        10_000_000.0,
        25_000_000.0,
        50_000_000.0,
        100_000_000.0,
        250_000_000.0,
        500_000_000.0,
        1_000_000_000.0,
    ]
}

/// 撮合引擎核心指标
pub struct Metrics {
    /// 订单总数 (按类型: buy/sell)
    pub orders_total: CounterVec,

    /// 成交总数
    pub trades_total: CounterVec,

    /// 订单取消总数
    pub cancellations_total: CounterVec,

    /// 撮合延迟分布 (纳秒)
    pub matching_latency_ns: HistogramVec,

    /// 订单在 WAL 缓冲中的 append 耗时 (纳秒)
    pub wal_latency_ns: HistogramVec,

    /// 每批 WAL flush/fsync 的耗时 (纳秒)
    pub commit_latency_ns: HistogramVec,

    /// 命令在撮合线程队列中的等待时间分布 (纳秒)
    pub actor_queue_wait_ns: HistogramVec,

    /// 从入队到收到响应的端到端延迟分布 (纳秒)
    pub request_latency_ns: HistogramVec,

    /// 网络请求解码耗时 (纳秒)
    pub network_decode_ns: HistogramVec,

    /// 响应编码耗时 (纳秒)
    pub network_encode_ns: HistogramVec,

    /// socket 发送耗时 (纳秒)
    pub socket_send_ns: HistogramVec,

    /// 从消息读取完成到响应发送完成的软件 socket-to-socket 延迟 (纳秒)
    pub socket_to_socket_ns: HistogramVec,

    /// 订单大小分布
    pub order_size: HistogramVec,

    /// Replication lag in WAL records (sent - min acked).
    pub replication_lag_records: IntGauge,

    /// Number of connected standby replicas.
    pub replication_connected: IntGauge,

    /// Batches dropped because a standby was too slow.
    pub replication_dropped_batches: IntCounter,
}

impl Metrics {
    /// 创建新的Metrics实例
    ///
    /// # 错误
    /// 如果Prometheus指标注册失败（例如重复注册），返回错误
    pub fn new() -> Result<Self, prometheus::Error> {
        Ok(Self {
            orders_total: register_counter_vec!(
                "matching_engine_orders_total",
                "Total number of orders received",
                &["order_type", "symbol"]
            )?,

            trades_total: register_counter_vec!(
                "matching_engine_trades_total",
                "Total number of trades executed",
                &["symbol"]
            )?,

            cancellations_total: register_counter_vec!(
                "matching_engine_cancellations_total",
                "Total number of order cancellations",
                &["symbol", "status"]
            )?,

            matching_latency_ns: register_histogram_vec!(
                "matching_engine_matching_latency_nanoseconds",
                "Order matching latency in nanoseconds",
                &["symbol"],
                latency_buckets()
            )?,

            wal_latency_ns: register_histogram_vec!(
                "matching_engine_wal_latency_nanoseconds",
                "WAL append latency in nanoseconds (before group commit)",
                &["symbol"],
                latency_buckets()
            )?,

            commit_latency_ns: register_histogram_vec!(
                "matching_engine_commit_latency_nanoseconds",
                "WAL flush/fsync latency per group commit, in nanoseconds",
                &["symbol"],
                latency_buckets()
            )?,

            actor_queue_wait_ns: register_histogram_vec!(
                "matching_engine_actor_queue_wait_nanoseconds",
                "Time an order waited in the matching actor queue, in nanoseconds",
                &["symbol"],
                latency_buckets()
            )?,

            request_latency_ns: register_histogram_vec!(
                "matching_engine_request_latency_nanoseconds",
                "End-to-end request latency from enqueue to reply, in nanoseconds",
                &["symbol"],
                latency_buckets()
            )?,

            network_decode_ns: register_histogram_vec!(
                "matching_engine_network_decode_nanoseconds",
                "Request decode latency in nanoseconds",
                &["symbol"],
                latency_buckets()
            )?,

            network_encode_ns: register_histogram_vec!(
                "matching_engine_network_encode_nanoseconds",
                "Response encode latency in nanoseconds",
                &["symbol"],
                latency_buckets()
            )?,

            socket_send_ns: register_histogram_vec!(
                "matching_engine_socket_send_nanoseconds",
                "Socket send latency in nanoseconds",
                &["symbol"],
                latency_buckets()
            )?,

            socket_to_socket_ns: register_histogram_vec!(
                "matching_engine_socket_to_socket_nanoseconds",
                "Software socket-to-socket latency (read complete to send complete), in nanoseconds",
                &["symbol"],
                latency_buckets()
            )?,

            order_size: register_histogram_vec!(
                "matching_engine_order_size",
                "Order size distribution",
                &["order_type"],
                vec![1.0, 10.0, 100.0, 1000.0, 10_000.0, 100_000.0]
            )?,
            replication_lag_records: register_int_gauge!(
                "matching_engine_replication_lag_records",
                "WAL records between sent and minimum acked standby position"
            )?,
            replication_connected: register_int_gauge!(
                "matching_engine_replication_connected",
                "Number of connected standby replicas"
            )?,
            replication_dropped_batches: register_int_counter!(
                "matching_engine_replication_dropped_batches",
                "Replication batches dropped because a standby was too slow"
            )?,
        })
    }

    /// 导出Prometheus格式的指标
    ///
    /// # 错误
    /// 如果编码失败或输出不是有效的 UTF-8，返回错误
    pub fn export(&self) -> Result<String, MetricsError> {
        let encoder = TextEncoder::new();
        let metric_families = prometheus::gather();
        let mut buffer = vec![];
        encoder.encode(&metric_families, &mut buffer)?;
        let result = String::from_utf8(buffer)?;
        Ok(result)
    }

    /// 重置所有指标（仅用于测试）
    #[cfg(test)]
    pub fn reset(&self) {
        self.orders_total.reset();
        self.trades_total.reset();
        self.cancellations_total.reset();
        self.matching_latency_ns.reset();
        self.wal_latency_ns.reset();
        self.commit_latency_ns.reset();
        self.actor_queue_wait_ns.reset();
        self.request_latency_ns.reset();
        self.network_decode_ns.reset();
        self.network_encode_ns.reset();
        self.socket_send_ns.reset();
        self.socket_to_socket_ns.reset();
        self.order_size.reset();
        self.replication_lag_records.set(0);
        self.replication_connected.set(0);
        self.replication_dropped_batches.reset();
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
            .expect("Failed to create default Metrics: Prometheus metric registration failed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_global() {
        // 使用全局METRICS实例而不是创建新的
        METRICS
            .orders_total
            .with_label_values(&["buy", "TEST"])
            .inc();

        // 测试导出
        let output = METRICS.export().expect("Failed to export metrics");
        assert!(output.contains("matching_engine_orders_total"));
    }

    #[test]
    fn test_histogram_global() {
        METRICS
            .matching_latency_ns
            .with_label_values(&["TEST"])
            .observe(125.0);

        let output = METRICS.export().expect("Failed to export metrics");
        assert!(output.contains("matching_engine_matching_latency_nanoseconds"));
    }

    #[test]
    fn test_cancellation_counter() {
        METRICS
            .cancellations_total
            .with_label_values(&["TEST", "ok"])
            .inc();

        let output = METRICS.export().expect("Failed to export metrics");
        assert!(output.contains("matching_engine_cancellations_total"));
    }
}
