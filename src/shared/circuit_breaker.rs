//! Circuit Breaker Module - 熔断器模式实现
//!
//! 提供熔断器保护机制，防止故障级联传播。
//!
//! ## 功能特性
//! - **三种状态**: Closed（正常）、Open（熔断）、HalfOpen（半开）
//! - **自动恢复**: 熔断后自动尝试恢复
//! - **失败阈值**: 可配置的失败次数阈值
//! - **超时配置**: 熔断后的恢复等待时间
//! - **线程安全**: 使用原子操作保证并发安全
//!
//! ## 工作原理
//! 1. **Closed 状态**: 正常处理请求，统计失败次数
//! 2. **Open 状态**: 达到失败阈值后进入，快速失败所有请求
//! 3. **HalfOpen 状态**: 超时后尝试恢复，允许少量请求测试
//!
//! ## 使用示例
//! ```rust
//! use matching_engine::shared::circuit_breaker::{CircuitBreaker, CircuitBreakerConfig};
//! use std::time::Duration;
//!
//! // 创建熔断器：5次失败后熔断，30秒后尝试恢复
//! let config = CircuitBreakerConfig {
//!     failure_threshold: 5,
//!     success_threshold: 2,
//!     timeout: Duration::from_secs(30),
//! };
//! let breaker = CircuitBreaker::new(config);
//!
//! // 执行受保护的操作
//! match breaker.call(|| perform_risky_operation()) {
//!     Ok(result) => println!("Success: {:?}", result),
//!     Err(e) => println!("Failed or circuit open: {:?}", e),
//! }
//! ```

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, warn};

/// 熔断器状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// 关闭状态（正常工作）
    Closed,
    /// 打开状态（熔断中，拒绝所有请求）
    Open,
    /// 半开状态（尝试恢复，允许部分请求）
    HalfOpen,
}

/// 熔断器配置
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// 失败次数阈值（达到后进入 Open 状态）
    pub failure_threshold: usize,

    /// 成功次数阈值（HalfOpen 状态下，达到后恢复到 Closed）
    pub success_threshold: usize,

    /// 熔断超时时间（Open 状态持续时间）
    pub timeout: Duration,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            success_threshold: 2,
            timeout: Duration::from_secs(60),
        }
    }
}

/// 熔断器实现
///
/// 使用原子操作实现无锁并发控制
pub struct CircuitBreaker {
    /// 配置
    config: CircuitBreakerConfig,

    /// 当前状态（使用 u8 表示：0=Closed, 1=Open, 2=HalfOpen）
    state: Arc<AtomicUsize>,

    /// 连续失败计数
    failure_count: Arc<AtomicUsize>,

    /// 连续成功计数（HalfOpen 状态下使用）
    success_count: Arc<AtomicUsize>,

    /// 上次状态变化时间（使用纳秒表示）
    last_failure_time: Arc<AtomicU64>,

    /// 总请求数（统计用）
    total_requests: Arc<AtomicUsize>,

    /// 总失败数（统计用）
    total_failures: Arc<AtomicUsize>,
}

impl CircuitBreaker {
    /// 创建新的熔断器
    ///
    /// # 参数
    /// - `config`: 熔断器配置
    #[must_use]
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            config,
            state: Arc::new(AtomicUsize::new(CircuitState::Closed as usize)),
            failure_count: Arc::new(AtomicUsize::new(0)),
            success_count: Arc::new(AtomicUsize::new(0)),
            last_failure_time: Arc::new(AtomicU64::new(0)),
            total_requests: Arc::new(AtomicUsize::new(0)),
            total_failures: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// 获取当前状态
    #[must_use]
    pub fn state(&self) -> CircuitState {
        match self.state.load(Ordering::Relaxed) {
            0 => CircuitState::Closed,
            1 => CircuitState::Open,
            2 => CircuitState::HalfOpen,
            _ => CircuitState::Closed, // 默认值
        }
    }

    /// 执行受保护的操作
    ///
    /// # 参数
    /// - `operation`: 要执行的操作（闭包）
    ///
    /// # 返回
    /// - `Ok(T)`: 操作成功
    /// - `Err(CircuitBreakerError)`: 熔断器打开或操作失败
    pub fn call<F, T, E>(&self, operation: F) -> Result<T, CircuitBreakerError<E>>
    where
        F: FnOnce() -> Result<T, E>,
    {
        self.total_requests.fetch_add(1, Ordering::Relaxed);

        // 检查是否需要从 Open 转换到 HalfOpen
        self.check_and_update_state();

        let current_state = self.state();

        match current_state {
            CircuitState::Open => {
                // 熔断状态，快速失败
                Err(CircuitBreakerError::CircuitOpen)
            }
            CircuitState::Closed | CircuitState::HalfOpen => {
                // 执行操作
                match operation() {
                    Ok(result) => {
                        self.on_success();
                        Ok(result)
                    }
                    Err(e) => {
                        self.on_failure();
                        Err(CircuitBreakerError::OperationFailed(e))
                    }
                }
            }
        }
    }

    /// 检查并更新状态
    fn check_and_update_state(&self) {
        let current_state = self.state();

        if current_state == CircuitState::Open {
            let last_failure = self.last_failure_time.load(Ordering::Relaxed);
            let now = Self::current_time_nanos();
            let elapsed = Duration::from_nanos(now - last_failure);

            if elapsed >= self.config.timeout {
                // 超时，尝试恢复到 HalfOpen
                debug!("Circuit breaker transitioning from Open to HalfOpen");
                self.state
                    .store(CircuitState::HalfOpen as usize, Ordering::Relaxed);
                self.success_count.store(0, Ordering::Relaxed);
            }
        }
    }

    /// 操作成功时的处理
    fn on_success(&self) {
        let current_state = self.state();

        match current_state {
            CircuitState::Closed => {
                // 重置失败计数
                self.failure_count.store(0, Ordering::Relaxed);
            }
            CircuitState::HalfOpen => {
                let successes = self.success_count.fetch_add(1, Ordering::Relaxed) + 1;

                if successes >= self.config.success_threshold {
                    // 成功次数达到阈值，恢复到 Closed
                    debug!(
                        "Circuit breaker recovered: {} consecutive successes",
                        successes
                    );
                    self.state
                        .store(CircuitState::Closed as usize, Ordering::Relaxed);
                    self.failure_count.store(0, Ordering::Relaxed);
                    self.success_count.store(0, Ordering::Relaxed);
                }
            }
            CircuitState::Open => {
                // 不应该到达这里
            }
        }
    }

    /// 操作失败时的处理
    fn on_failure(&self) {
        self.total_failures.fetch_add(1, Ordering::Relaxed);
        let current_state = self.state();

        match current_state {
            CircuitState::Closed => {
                let failures = self.failure_count.fetch_add(1, Ordering::Relaxed) + 1;

                if failures >= self.config.failure_threshold {
                    // 失败次数达到阈值，进入 Open 状态
                    warn!("Circuit breaker opened: {} consecutive failures", failures);
                    self.state
                        .store(CircuitState::Open as usize, Ordering::Relaxed);
                    self.last_failure_time
                        .store(Self::current_time_nanos(), Ordering::Relaxed);
                }
            }
            CircuitState::HalfOpen => {
                // HalfOpen 状态下失败，立即回到 Open
                warn!("Circuit breaker failed during half-open state, reopening");
                self.state
                    .store(CircuitState::Open as usize, Ordering::Relaxed);
                self.last_failure_time
                    .store(Self::current_time_nanos(), Ordering::Relaxed);
                self.success_count.store(0, Ordering::Relaxed);
            }
            CircuitState::Open => {
                // 已经是 Open 状态，更新时间
                self.last_failure_time
                    .store(Self::current_time_nanos(), Ordering::Relaxed);
            }
        }
    }

    /// 获取当前时间（纳秒）
    /// 使用 SystemTime 以获得绝对时间戳
    fn current_time_nanos() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};

        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
    }

    /// 获取统计信息
    #[must_use]
    pub fn stats(&self) -> CircuitBreakerStats {
        CircuitBreakerStats {
            state: self.state(),
            total_requests: self.total_requests.load(Ordering::Relaxed),
            total_failures: self.total_failures.load(Ordering::Relaxed),
            failure_count: self.failure_count.load(Ordering::Relaxed),
            success_count: self.success_count.load(Ordering::Relaxed),
        }
    }

    /// 手动重置熔断器（仅用于测试或紧急恢复）
    pub fn reset(&self) {
        self.state
            .store(CircuitState::Closed as usize, Ordering::Relaxed);
        self.failure_count.store(0, Ordering::Relaxed);
        self.success_count.store(0, Ordering::Relaxed);
        self.last_failure_time.store(0, Ordering::Relaxed);
    }
}

/// 熔断器统计信息
#[derive(Debug, Clone)]
pub struct CircuitBreakerStats {
    /// 当前状态
    pub state: CircuitState,
    /// 总请求数
    pub total_requests: usize,
    /// 总失败数
    pub total_failures: usize,
    /// 当前连续失败数
    pub failure_count: usize,
    /// 当前连续成功数（HalfOpen 状态）
    pub success_count: usize,
}

/// 熔断器错误类型
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CircuitBreakerError<E> {
    /// 熔断器打开（熔断中）
    CircuitOpen,
    /// 操作失败
    OperationFailed(E),
}

impl<E: std::fmt::Display> std::fmt::Display for CircuitBreakerError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::CircuitOpen => write!(f, "Circuit breaker is open"),
            Self::OperationFailed(e) => write!(f, "Operation failed: {}", e),
        }
    }
}

impl<E: std::error::Error> std::error::Error for CircuitBreakerError<E> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_circuit_breaker_closed_state() {
        let config = CircuitBreakerConfig {
            failure_threshold: 3,
            success_threshold: 2,
            timeout: Duration::from_secs(1),
        };
        let breaker = CircuitBreaker::new(config);

        assert_eq!(breaker.state(), CircuitState::Closed);

        // 成功的操作
        let result = breaker.call(|| Ok::<_, String>(42));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
        assert_eq!(breaker.state(), CircuitState::Closed);
    }

    #[test]
    fn test_circuit_breaker_opens_on_failures() {
        let config = CircuitBreakerConfig {
            failure_threshold: 3,
            success_threshold: 2,
            timeout: Duration::from_millis(100),
        };
        let breaker = CircuitBreaker::new(config);

        // 触发3次失败
        for _ in 0..3 {
            let _ = breaker.call(|| Err::<i32, _>("error"));
        }

        // 应该进入 Open 状态
        assert_eq!(breaker.state(), CircuitState::Open);

        // Open 状态下请求应该快速失败
        let result = breaker.call(|| Ok::<_, String>(42));
        assert!(matches!(result, Err(CircuitBreakerError::CircuitOpen)));
    }

    #[test]
    fn test_circuit_breaker_half_open_recovery() {
        let config = CircuitBreakerConfig {
            failure_threshold: 2,
            success_threshold: 2,
            timeout: Duration::from_millis(50),
        };
        let breaker = CircuitBreaker::new(config);

        // 触发失败，进入 Open
        for _ in 0..2 {
            let _ = breaker.call(|| Err::<i32, _>("error"));
        }
        assert_eq!(breaker.state(), CircuitState::Open);

        // 等待超时
        std::thread::sleep(Duration::from_millis(60));

        // 检查状态更新（需要调用 call 来触发检查）
        let _ = breaker.call(|| Ok::<_, String>(1));
        assert_eq!(breaker.state(), CircuitState::HalfOpen);

        // 再次成功，应该恢复到 Closed
        let _ = breaker.call(|| Ok::<_, String>(2));
        assert_eq!(breaker.state(), CircuitState::Closed);
    }

    #[test]
    fn test_circuit_breaker_half_open_failure() {
        let config = CircuitBreakerConfig {
            failure_threshold: 2,
            success_threshold: 2,
            timeout: Duration::from_millis(50),
        };
        let breaker = CircuitBreaker::new(config);

        // 触发失败，进入 Open
        for _ in 0..2 {
            let _ = breaker.call(|| Err::<i32, _>("error"));
        }

        // 等待超时
        std::thread::sleep(Duration::from_millis(60));

        // 触发状态检查
        let _ = breaker.call(|| Ok::<_, String>(1));
        assert_eq!(breaker.state(), CircuitState::HalfOpen);

        // HalfOpen 状态下失败，应该回到 Open
        let _ = breaker.call(|| Err::<i32, _>("error"));
        assert_eq!(breaker.state(), CircuitState::Open);
    }

    #[test]
    fn test_circuit_breaker_stats() {
        let config = CircuitBreakerConfig::default();
        let breaker = CircuitBreaker::new(config);

        // 执行一些操作
        let _ = breaker.call(|| Ok::<_, String>(1));
        let _ = breaker.call(|| Ok::<_, String>(2));
        let _ = breaker.call(|| Err::<i32, _>("error"));

        let stats = breaker.stats();
        assert_eq!(stats.total_requests, 3);
        assert_eq!(stats.total_failures, 1);
        assert_eq!(stats.state, CircuitState::Closed);
    }

    #[test]
    fn test_circuit_breaker_reset() {
        let config = CircuitBreakerConfig {
            failure_threshold: 2,
            success_threshold: 2,
            timeout: Duration::from_secs(1),
        };
        let breaker = CircuitBreaker::new(config);

        // 触发失败，进入 Open
        for _ in 0..2 {
            let _ = breaker.call(|| Err::<i32, _>("error"));
        }
        assert_eq!(breaker.state(), CircuitState::Open);

        // 手动重置
        breaker.reset();
        assert_eq!(breaker.state(), CircuitState::Closed);

        // 应该可以正常工作
        let result = breaker.call(|| Ok::<_, String>(42));
        assert!(result.is_ok());
    }
}
