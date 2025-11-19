//! Observability Module
//!
//! 提供系统可观测性功能：
//! - Prometheus metrics导出
//! - 健康检查端点
//! - 运行时指标收集
//! - OpenTelemetry tracing（可选）
//!
//! ## 模块结构
//! - `health` - 健康检查
//! - `http_server` - HTTP可观测性服务器
//! - `metrics_collector` - 运行时指标收集
pub mod health;
pub mod http_server;
pub mod metrics_collector;

pub use health::{HealthChecker, HealthResponse, HealthStatus, HealthDetails};
pub use http_server::ObservabilityServer;
pub use metrics_collector::{MetricsCollector, GLOBAL_METRICS};
