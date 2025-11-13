//! Health Check Endpoint
//!
//! 提供系统健康状态检查，用于负载均衡器和监控系统
//!
//! ## 健康检查端点
//! - `/health` - 简单的存活检查
//! - `/health/ready` - 就绪检查（系统是否可以接受流量）
//! - `/health/live` - 存活检查（系统是否仍在运行）
//!
//! ## 响应格式
//! ```json
//! {
//!   "status": "healthy",
//!   "uptime_seconds": 3600,
//!   "version": "0.1.0",
//!   "timestamp": 1234567890
//! }
//! ```

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use parking_lot::RwLock;

/// 健康状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    /// 健康
    Healthy,
    /// 降级（部分功能不可用）
    Degraded,
    /// 不健康
    Unhealthy,
}

/// 健康检查响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    /// 状态
    pub status: HealthStatus,
    /// 运行时间（秒）
    pub uptime_seconds: u64,
    /// 版本号
    pub version: String,
    /// 时间戳
    pub timestamp: u64,
    /// 详细信息（可选）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<HealthDetails>,
}

/// 详细健康信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthDetails {
    /// 内存使用率 (0-100)
    pub memory_usage_percent: f64,
    /// 活跃连接数
    pub active_connections: usize,
    /// 总处理订单数
    pub total_orders: u64,
    /// 总成交数
    pub total_trades: u64,
    /// 平均延迟（微秒）
    pub avg_latency_us: f64,
}

/// 健康检查器
pub struct HealthChecker {
    /// 启动时间
    start_time: SystemTime,
    /// 当前状态
    status: Arc<RwLock<HealthStatus>>,
    /// 版本号
    version: String,
}

impl HealthChecker {
    /// 创建新的健康检查器
    pub fn new(version: impl Into<String>) -> Self {
        Self {
            start_time: SystemTime::now(),
            status: Arc::new(RwLock::new(HealthStatus::Healthy)),
            version: version.into(),
        }
    }

    /// 获取运行时间（秒）
    pub fn uptime_seconds(&self) -> u64 {
        self.start_time
            .elapsed()
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// 获取当前时间戳
    fn current_timestamp() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// 设置健康状态
    pub fn set_status(&self, status: HealthStatus) {
        *self.status.write() = status;
    }

    /// 获取健康状态
    pub fn get_status(&self) -> HealthStatus {
        *self.status.read()
    }

    /// 生成健康检查响应
    pub fn check_health(&self) -> HealthResponse {
        HealthResponse {
            status: self.get_status(),
            uptime_seconds: self.uptime_seconds(),
            version: self.version.clone(),
            timestamp: Self::current_timestamp(),
            details: None,
        }
    }

    /// 生成详细健康检查响应
    pub fn check_health_detailed(&self, details: HealthDetails) -> HealthResponse {
        HealthResponse {
            status: self.get_status(),
            uptime_seconds: self.uptime_seconds(),
            version: self.version.clone(),
            timestamp: Self::current_timestamp(),
            details: Some(details),
        }
    }

    /// 存活检查（liveness probe）
    /// 返回true表示进程仍在运行
    pub fn check_liveness(&self) -> bool {
        // 简单检查：只要能返回就说明还活着
        true
    }

    /// 就绪检查（readiness probe）
    /// 返回true表示可以接受流量
    pub fn check_readiness(&self) -> bool {
        matches!(self.get_status(), HealthStatus::Healthy)
    }
}

impl Default for HealthChecker {
    fn default() -> Self {
        Self::new("0.1.0")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_health_checker_creation() {
        let checker = HealthChecker::new("1.0.0");
        assert_eq!(checker.version, "1.0.0");
        assert_eq!(checker.get_status(), HealthStatus::Healthy);
    }

    #[test]
    fn test_uptime() {
        let checker = HealthChecker::new("1.0.0");
        thread::sleep(Duration::from_millis(100));
        assert!(checker.uptime_seconds() >= 0);
    }

    #[test]
    fn test_status_change() {
        let checker = HealthChecker::new("1.0.0");
        assert_eq!(checker.get_status(), HealthStatus::Healthy);

        checker.set_status(HealthStatus::Degraded);
        assert_eq!(checker.get_status(), HealthStatus::Degraded);

        checker.set_status(HealthStatus::Unhealthy);
        assert_eq!(checker.get_status(), HealthStatus::Unhealthy);
    }

    #[test]
    fn test_health_response() {
        let checker = HealthChecker::new("1.0.0");
        let response = checker.check_health();

        assert_eq!(response.status, HealthStatus::Healthy);
        assert_eq!(response.version, "1.0.0");
        assert!(response.uptime_seconds >= 0);
        assert!(response.timestamp > 0);
        assert!(response.details.is_none());
    }

    #[test]
    fn test_health_response_detailed() {
        let checker = HealthChecker::new("1.0.0");
        let details = HealthDetails {
            memory_usage_percent: 45.2,
            active_connections: 10,
            total_orders: 1000,
            total_trades: 500,
            avg_latency_us: 12.5,
        };

        let response = checker.check_health_detailed(details.clone());
        assert_eq!(response.status, HealthStatus::Healthy);
        assert!(response.details.is_some());

        let resp_details = response.details.unwrap();
        assert_eq!(resp_details.memory_usage_percent, 45.2);
        assert_eq!(resp_details.active_connections, 10);
    }

    #[test]
    fn test_liveness_probe() {
        let checker = HealthChecker::new("1.0.0");
        assert!(checker.check_liveness());

        // 即使状态不健康，存活检查也应该通过
        checker.set_status(HealthStatus::Unhealthy);
        assert!(checker.check_liveness());
    }

    #[test]
    fn test_readiness_probe() {
        let checker = HealthChecker::new("1.0.0");
        assert!(checker.check_readiness());

        checker.set_status(HealthStatus::Degraded);
        assert!(!checker.check_readiness());

        checker.set_status(HealthStatus::Unhealthy);
        assert!(!checker.check_readiness());

        checker.set_status(HealthStatus::Healthy);
        assert!(checker.check_readiness());
    }

    #[test]
    fn test_serialization() {
        let response = HealthResponse {
            status: HealthStatus::Healthy,
            uptime_seconds: 3600,
            version: "1.0.0".to_string(),
            timestamp: 1234567890,
            details: None,
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("healthy"));
        assert!(json.contains("3600"));
        assert!(json.contains("1.0.0"));
    }
}
