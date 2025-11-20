//! Prometheus Metrics Module
//!
//! 提供撮合引擎的核心性能指标监控
//!
//! ## 指标类型
//! - **Counter**: 订单总数、成交总数
//! - **Histogram**: 撮合延迟、订单大小
//! - **Gauge**: 订单簿深度、活跃连接数
//!
//! ## 使用示例
//! ```rust,ignore
//! use matching_engine::shared::metrics::METRICS;
//!
//! // 记录新订单
//! METRICS.orders_total.inc();
//!
//! // 记录撮合延迟
//! let timer = METRICS.matching_duration.start_timer();
//! // ... 执行撮合 ...
//! timer.observe_duration();
//! ```

use lazy_static::lazy_static;
use prometheus::{
    register_counter_vec, register_gauge_vec, register_histogram_vec, CounterVec, Encoder,
    GaugeVec, HistogramVec, TextEncoder,
};

lazy_static! {
    /// 全局Metrics实例
    ///
    /// # Panics
    /// 如果Prometheus指标注册失败，将在初始化时panic。
    /// 这通常发生在指标被重复注册时，表示程序配置错误。
    pub static ref METRICS: Metrics = Metrics::new()
        .expect("Failed to initialize Prometheus metrics. This usually indicates duplicate metric registration.");
}

/// 撮合引擎核心指标
pub struct Metrics {
    /// 订单总数 (按类型: buy/sell)
    pub orders_total: CounterVec,

    /// 成交总数
    pub trades_total: CounterVec,

    /// 订单取消总数
    pub cancellations_total: CounterVec,

    /// 撮合延迟分布 (微秒)
    pub matching_duration: HistogramVec,

    /// 订单大小分布
    pub order_size: HistogramVec,

    /// 订单簿深度 (买/卖)
    pub orderbook_depth: GaugeVec,

    /// 活跃连接数
    pub active_connections: GaugeVec,

    /// 消息处理速率 (msg/s)
    pub message_rate: CounterVec,

    /// 错误总数 (按类型)
    pub errors_total: CounterVec,

    /// 分区负载 (每个分区的订单数)
    pub partition_load: GaugeVec,
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

            matching_duration: register_histogram_vec!(
                "matching_engine_matching_duration_microseconds",
                "Order matching duration in microseconds",
                &["symbol"],
                vec![1.0, 5.0, 10.0, 50.0, 100.0, 500.0, 1000.0, 5000.0]
            )?,

            order_size: register_histogram_vec!(
                "matching_engine_order_size",
                "Order size distribution",
                &["order_type"],
                vec![1.0, 10.0, 100.0, 1000.0, 10_000.0, 100_000.0]
            )?,

            orderbook_depth: register_gauge_vec!(
                "matching_engine_orderbook_depth",
                "Current orderbook depth",
                &["symbol", "side"]
            )?,

            active_connections: register_gauge_vec!(
                "matching_engine_active_connections",
                "Number of active client connections",
                &["status"]
            )?,

            message_rate: register_counter_vec!(
                "matching_engine_messages_total",
                "Total number of messages processed",
                &["message_type"]
            )?,

            errors_total: register_counter_vec!(
                "matching_engine_errors_total",
                "Total number of errors",
                &["error_type"]
            )?,

            partition_load: register_gauge_vec!(
                "matching_engine_partition_load",
                "Number of orders per partition",
                &["partition_id"]
            )?,
        })
    }

    /// 导出Prometheus格式的指标
    ///
    /// # 错误
    /// 如果编码失败或输出不是有效的 UTF-8，返回错误
    pub fn export(&self) -> Result<String, Box<dyn std::error::Error>> {
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
        self.orderbook_depth.reset();
        self.active_connections.reset();
        self.message_rate.reset();
        self.errors_total.reset();
        self.partition_load.reset();
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
        // 使用全局实例
        METRICS
            .matching_duration
            .with_label_values(&["TEST"])
            .observe(125.5);

        let output = METRICS.export().expect("Failed to export metrics");
        assert!(output.contains("matching_engine_matching_duration_microseconds"));
    }

    #[test]
    fn test_gauge_global() {
        // 使用全局实例
        METRICS
            .orderbook_depth
            .with_label_values(&["TEST", "bid"])
            .set(100.0);

        // Note: 由于是全局共享，不能假设值精确匹配
        let output = METRICS.export().expect("Failed to export metrics");
        assert!(output.contains("matching_engine_orderbook_depth"));
    }
}
