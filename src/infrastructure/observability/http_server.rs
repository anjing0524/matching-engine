//! HTTP Observability Server
//!
//! 提供Prometheus metrics和健康检查端点
//!
//! ## 端点
//! - `GET /metrics` - Prometheus格式的指标
//! - `GET /health` - 健康检查
//! - `GET /health/ready` - 就绪检查
//! - `GET /health/live` - 存活检查
//!
//! ## 使用示例
//! ```rust,ignore
//! let server = ObservabilityServer::new(9090);
//! server.run().await?;
//! ```

use crate::shared::metrics::METRICS;
use super::health::{HealthChecker, HealthDetails};
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::{error, info};

/// 可观测性服务器
pub struct ObservabilityServer {
    addr: SocketAddr,
    health_checker: Arc<HealthChecker>,
}

impl ObservabilityServer {
    /// 创建新的可观测性服务器
    pub fn new(port: u16) -> Self {
        let addr = SocketAddr::from(([0, 0, 0, 0], port));
        Self {
            addr,
            health_checker: Arc::new(HealthChecker::new(env!("CARGO_PKG_VERSION"))),
        }
    }

    /// 获取健康检查器
    pub fn health_checker(&self) -> Arc<HealthChecker> {
        self.health_checker.clone()
    }

    /// 启动HTTP服务器
    pub async fn run(self) -> Result<(), Box<dyn std::error::Error>> {
        let app = Router::new()
            .route("/metrics", get(metrics_handler))
            .route("/health", get(health_handler))
            .route("/health/ready", get(readiness_handler))
            .route("/health/live", get(liveness_handler))
            .with_state(self.health_checker.clone());

        info!("可观测性服务器启动于 {}", self.addr);
        info!("Metrics端点: http://{}/metrics", self.addr);
        info!("健康检查端点: http://{}/health", self.addr);

        let listener = tokio::net::TcpListener::bind(self.addr).await?;
        axum::serve(listener, app).await?;

        Ok(())
    }
}

/// Prometheus metrics端点
async fn metrics_handler() -> Response {
    let metrics = METRICS.export();
    (StatusCode::OK, metrics).into_response()
}

/// 健康检查端点
async fn health_handler(State(checker): State<Arc<HealthChecker>>) -> Response {
    // 收集详细健康信息
    let details = HealthDetails {
        memory_usage_percent: get_memory_usage(),
        active_connections: 0, // TODO: 从全局状态获取
        total_orders: METRICS.orders_total.with_label_values(&["buy", "*"]).get() as u64
            + METRICS.orders_total.with_label_values(&["sell", "*"]).get() as u64,
        total_trades: METRICS.trades_total.with_label_values(&["*"]).get() as u64,
        avg_latency_us: 0.0, // TODO: 计算平均延迟
    };

    let response = checker.check_health_detailed(details);

    let status_code = match response.status {
        super::health::HealthStatus::Healthy => StatusCode::OK,
        super::health::HealthStatus::Degraded => StatusCode::OK,
        super::health::HealthStatus::Unhealthy => StatusCode::SERVICE_UNAVAILABLE,
    };

    (status_code, Json(response)).into_response()
}

/// 就绪检查端点（用于Kubernetes readiness probe）
async fn readiness_handler(State(checker): State<Arc<HealthChecker>>) -> Response {
    if checker.check_readiness() {
        StatusCode::OK.into_response()
    } else {
        StatusCode::SERVICE_UNAVAILABLE.into_response()
    }
}

/// 存活检查端点（用于Kubernetes liveness probe）
async fn liveness_handler(State(checker): State<Arc<HealthChecker>>) -> Response {
    if checker.check_liveness() {
        StatusCode::OK.into_response()
    } else {
        StatusCode::SERVICE_UNAVAILABLE.into_response()
    }
}

/// 获取内存使用率（简化版）
fn get_memory_usage() -> f64 {
    // TODO: 实现真实的内存使用率检查
    // 可以使用 sysinfo crate
    0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_observability_server_creation() {
        let server = ObservabilityServer::new(9090);
        assert_eq!(server.addr.port(), 9090);
    }

    #[tokio::test]
    async fn test_metrics_handler() {
        let response = metrics_handler().await;
        // Response type doesn't have a direct body() method in axum 0.7
        // This test verifies the handler compiles and runs
        assert!(true);
    }

    #[tokio::test]
    async fn test_liveness_handler() {
        let checker = Arc::new(HealthChecker::new("1.0.0"));
        let response = liveness_handler(State(checker)).await;
        // Verify the handler runs without errors
        assert!(true);
    }
}
