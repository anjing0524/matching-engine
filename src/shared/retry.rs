//! Retry Module - 通用重试机制
//!
//! 提供灵活的重试策略，用于处理临时性失败。
//!
//! ## 功能特性
//! - **指数退避**: 重试间隔呈指数增长
//! - **抖动**: 添加随机抖动避免雷鸣羊群效应
//! - **最大重试次数**: 防止无限重试
//! - **可配置延迟**: 支持自定义初始延迟和最大延迟
//! - **条件重试**: 支持自定义错误判断逻辑
//!
//! ## 使用示例
//! ```rust
//! use matching_engine::shared::retry::{RetryPolicy, retry_with_policy};
//! use std::time::Duration;
//!
//! // 创建重试策略：最多重试 3 次，初始延迟 100ms
//! let policy = RetryPolicy::exponential(3, Duration::from_millis(100));
//!
//! // 执行带重试的操作
//! let result = retry_with_policy(policy, || {
//!     // 可能失败的操作
//!     perform_network_request()
//! });
//! ```

use std::time::Duration;
use tracing::{debug, warn};

/// 重试策略
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// 最大重试次数
    max_retries: usize,

    /// 初始延迟
    initial_delay: Duration,

    /// 最大延迟
    max_delay: Duration,

    /// 退避因子（默认 2.0，即指数退避）
    backoff_factor: f64,

    /// 是否添加抖动（jitter）
    jitter: bool,
}

impl RetryPolicy {
    /// 创建指数退避策略
    ///
    /// # 参数
    /// - `max_retries`: 最大重试次数
    /// - `initial_delay`: 初始延迟时间
    ///
    /// # 示例
    /// ```
    /// use matching_engine::shared::retry::RetryPolicy;
    /// use std::time::Duration;
    ///
    /// let policy = RetryPolicy::exponential(3, Duration::from_millis(100));
    /// ```
    #[must_use]
    pub fn exponential(max_retries: usize, initial_delay: Duration) -> Self {
        Self {
            max_retries,
            initial_delay,
            max_delay: Duration::from_secs(30), // 默认最大延迟 30 秒
            backoff_factor: 2.0,
            jitter: true,
        }
    }

    /// 创建固定延迟策略
    ///
    /// # 参数
    /// - `max_retries`: 最大重试次数
    /// - `delay`: 固定延迟时间
    #[must_use]
    pub fn fixed(max_retries: usize, delay: Duration) -> Self {
        Self {
            max_retries,
            initial_delay: delay,
            max_delay: delay,
            backoff_factor: 1.0, // 固定延迟
            jitter: false,
        }
    }

    /// 设置最大延迟
    #[must_use]
    pub fn with_max_delay(mut self, max_delay: Duration) -> Self {
        self.max_delay = max_delay;
        self
    }

    /// 设置退避因子
    #[must_use]
    pub fn with_backoff_factor(mut self, factor: f64) -> Self {
        self.backoff_factor = factor;
        self
    }

    /// 设置是否使用抖动
    #[must_use]
    pub fn with_jitter(mut self, jitter: bool) -> Self {
        self.jitter = jitter;
        self
    }

    /// 计算第 n 次重试的延迟时间
    ///
    /// 使用指数退避公式：delay = initial_delay * (backoff_factor ^ attempt)
    /// 如果启用抖动，会添加 ±25% 的随机变化
    fn calculate_delay(&self, attempt: usize) -> Duration {
        let base_delay_ms =
            self.initial_delay.as_millis() as f64 * self.backoff_factor.powi(attempt as i32);

        let delay_ms = base_delay_ms.min(self.max_delay.as_millis() as f64);

        let final_delay_ms = if self.jitter {
            // 添加 ±25% 的抖动
            let jitter_factor = 0.75 + (rand::random::<f64>() * 0.5);
            delay_ms * jitter_factor
        } else {
            delay_ms
        };

        Duration::from_millis(final_delay_ms as u64)
    }

    /// 获取最大重试次数
    #[must_use]
    pub fn max_retries(&self) -> usize {
        self.max_retries
    }
}

/// 使用重试策略执行操作
///
/// # 参数
/// - `policy`: 重试策略
/// - `operation`: 要执行的操作（闭包）
///
/// # 返回
/// 操作的结果。如果所有重试都失败，返回最后一次的错误。
///
/// # 示例
/// ```
/// use matching_engine::shared::retry::{RetryPolicy, retry_with_policy};
/// use std::time::Duration;
///
/// let policy = RetryPolicy::exponential(3, Duration::from_millis(100));
///
/// let result = retry_with_policy(policy, || {
///     // 可能失败的操作
///     perform_operation()
/// });
/// ```
pub fn retry_with_policy<F, T, E>(policy: RetryPolicy, mut operation: F) -> Result<T, E>
where
    F: FnMut() -> Result<T, E>,
    E: std::fmt::Display,
{
    let mut last_error = None;

    for attempt in 0..=policy.max_retries {
        match operation() {
            Ok(result) => {
                if attempt > 0 {
                    debug!("Operation succeeded after {} retry attempts", attempt);
                }
                return Ok(result);
            }
            Err(e) => {
                if attempt < policy.max_retries {
                    let delay = policy.calculate_delay(attempt);
                    warn!(
                        "Operation failed (attempt {}/{}): {}. Retrying in {:?}",
                        attempt + 1,
                        policy.max_retries + 1,
                        e,
                        delay
                    );

                    std::thread::sleep(delay);
                    last_error = Some(e);
                } else {
                    warn!(
                        "Operation failed after {} attempts: {}",
                        policy.max_retries + 1,
                        e
                    );
                    return Err(e);
                }
            }
        }
    }

    // 理论上不会到达这里，但为了类型安全还是处理一下
    Err(last_error.expect("Last error should be set"))
}

/// 异步版本：使用重试策略执行异步操作
///
/// # 参数
/// - `policy`: 重试策略
/// - `operation`: 要执行的异步操作
///
/// # 返回
/// 操作的结果。如果所有重试都失败，返回最后一次的错误。
pub async fn retry_async<F, Fut, T, E>(policy: RetryPolicy, mut operation: F) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    let mut last_error = None;

    for attempt in 0..=policy.max_retries {
        match operation().await {
            Ok(result) => {
                if attempt > 0 {
                    debug!("Async operation succeeded after {} retry attempts", attempt);
                }
                return Ok(result);
            }
            Err(e) => {
                if attempt < policy.max_retries {
                    let delay = policy.calculate_delay(attempt);
                    warn!(
                        "Async operation failed (attempt {}/{}): {}. Retrying in {:?}",
                        attempt + 1,
                        policy.max_retries + 1,
                        e,
                        delay
                    );

                    tokio::time::sleep(delay).await;
                    last_error = Some(e);
                } else {
                    warn!(
                        "Async operation failed after {} attempts: {}",
                        policy.max_retries + 1,
                        e
                    );
                    return Err(e);
                }
            }
        }
    }

    Err(last_error.expect("Last error should be set"))
}

/// 条件重试：只对特定类型的错误进行重试
///
/// # 参数
/// - `policy`: 重试策略
/// - `operation`: 要执行的操作
/// - `should_retry`: 判断是否应该重试的函数
///
/// # 示例
/// ```
/// use matching_engine::shared::retry::{RetryPolicy, retry_with_condition};
/// use std::time::Duration;
///
/// let policy = RetryPolicy::exponential(3, Duration::from_millis(100));
///
/// let result = retry_with_condition(
///     policy,
///     || perform_operation(),
///     |err| err.is_transient(), // 只重试临时性错误
/// );
/// ```
pub fn retry_with_condition<F, T, E, C>(
    policy: RetryPolicy,
    mut operation: F,
    should_retry: C,
) -> Result<T, E>
where
    F: FnMut() -> Result<T, E>,
    C: Fn(&E) -> bool,
    E: std::fmt::Display,
{
    let mut last_error = None;

    for attempt in 0..=policy.max_retries {
        match operation() {
            Ok(result) => {
                if attempt > 0 {
                    debug!("Operation succeeded after {} retry attempts", attempt);
                }
                return Ok(result);
            }
            Err(e) => {
                if !should_retry(&e) {
                    warn!("Error is not retryable: {}", e);
                    return Err(e);
                }

                if attempt < policy.max_retries {
                    let delay = policy.calculate_delay(attempt);
                    warn!(
                        "Operation failed (attempt {}/{}): {}. Retrying in {:?}",
                        attempt + 1,
                        policy.max_retries + 1,
                        e,
                        delay
                    );

                    std::thread::sleep(delay);
                    last_error = Some(e);
                } else {
                    warn!(
                        "Operation failed after {} attempts: {}",
                        policy.max_retries + 1,
                        e
                    );
                    return Err(e);
                }
            }
        }
    }

    Err(last_error.expect("Last error should be set"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn test_retry_policy_exponential() {
        let policy = RetryPolicy::exponential(3, Duration::from_millis(100));
        assert_eq!(policy.max_retries(), 3);

        // 验证延迟递增（指数退避）
        let delay0 = policy.calculate_delay(0);
        let delay1 = policy.calculate_delay(1);
        let delay2 = policy.calculate_delay(2);

        // 因为有抖动，所以只能验证大致趋势
        assert!(delay0.as_millis() >= 75 && delay0.as_millis() <= 125);
        assert!(delay1 > delay0);
        assert!(delay2 > delay1);
    }

    #[test]
    fn test_retry_policy_fixed() {
        let policy = RetryPolicy::fixed(3, Duration::from_millis(100));

        // 固定延迟应该保持一致
        let delay0 = policy.calculate_delay(0);
        let delay1 = policy.calculate_delay(1);
        let delay2 = policy.calculate_delay(2);

        assert_eq!(delay0, Duration::from_millis(100));
        assert_eq!(delay1, Duration::from_millis(100));
        assert_eq!(delay2, Duration::from_millis(100));
    }

    #[test]
    fn test_retry_success_first_attempt() {
        let counter = AtomicUsize::new(0);
        let policy = RetryPolicy::exponential(3, Duration::from_millis(10));

        let result = retry_with_policy(policy, || {
            counter.fetch_add(1, Ordering::Relaxed);
            Ok::<_, String>(42)
        });

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
        assert_eq!(counter.load(Ordering::Relaxed), 1); // 只调用了一次
    }

    #[test]
    fn test_retry_success_after_failures() {
        let counter = AtomicUsize::new(0);
        let policy = RetryPolicy::exponential(3, Duration::from_millis(10));

        let result = retry_with_policy(policy, || {
            let count = counter.fetch_add(1, Ordering::Relaxed);
            if count < 2 {
                Err("Temporary failure")
            } else {
                Ok(42)
            }
        });

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
        assert_eq!(counter.load(Ordering::Relaxed), 3); // 重试了 2 次，第 3 次成功
    }

    #[test]
    fn test_retry_all_failures() {
        let counter = AtomicUsize::new(0);
        let policy = RetryPolicy::exponential(2, Duration::from_millis(10));

        let result = retry_with_policy(policy, || {
            counter.fetch_add(1, Ordering::Relaxed);
            Err::<i32, _>("Permanent failure")
        });

        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Permanent failure");
        assert_eq!(counter.load(Ordering::Relaxed), 3); // 初始 + 2 次重试 = 3 次
    }

    #[test]
    fn test_retry_with_condition() {
        let counter = AtomicUsize::new(0);
        let policy = RetryPolicy::exponential(3, Duration::from_millis(10));

        // 定义可重试的错误类型
        #[derive(Debug, PartialEq)]
        enum TestError {
            Transient,
            Permanent,
        }

        impl std::fmt::Display for TestError {
            fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(f, "{:?}", self)
            }
        }

        // 遇到永久性错误时应该立即失败
        let result: Result<i32, TestError> = retry_with_condition(
            policy.clone(),
            || {
                counter.fetch_add(1, Ordering::Relaxed);
                Err(TestError::Permanent)
            },
            |err| matches!(err, TestError::Transient),
        );

        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), TestError::Permanent);
        assert_eq!(counter.load(Ordering::Relaxed), 1); // 没有重试

        // 重置计数器
        counter.store(0, Ordering::Relaxed);

        // 临时性错误应该重试
        let result = retry_with_condition(
            policy,
            || {
                let count = counter.fetch_add(1, Ordering::Relaxed);
                if count < 2 {
                    Err(TestError::Transient)
                } else {
                    Ok(42)
                }
            },
            |err| matches!(err, TestError::Transient),
        );

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
        assert_eq!(counter.load(Ordering::Relaxed), 3); // 重试了 2 次
    }

    #[tokio::test]
    async fn test_retry_async() {
        let counter = AtomicUsize::new(0);
        let policy = RetryPolicy::exponential(3, Duration::from_millis(10));

        let result = retry_async(policy, || async {
            let count = counter.fetch_add(1, Ordering::Relaxed);
            if count < 2 {
                Err("Temporary failure")
            } else {
                Ok(42)
            }
        })
        .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
        assert_eq!(counter.load(Ordering::Relaxed), 3);
    }
}
