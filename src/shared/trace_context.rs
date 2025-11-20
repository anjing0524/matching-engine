//! Trace Context Module - 分布式追踪上下文
//!
//! 提供请求级别的追踪标识，用于日志关联和问题排查。
//!
//! ## 功能特性
//! - **唯一 Trace ID**: 每个请求生成唯一标识
//! - **线程本地存储**: 使用 thread_local 存储当前上下文
//! - **异步友好**: 支持 tokio 等异步运行时
//! - **零成本抽象**: 不使用时无性能开销
//!
//! ## 使用示例
//! ```rust
//! use matching_engine::shared::trace_context::{TraceContext, with_trace_context};
//!
//! // 创建新的 trace context
//! let ctx = TraceContext::new();
//! println!("Trace ID: {}", ctx.trace_id());
//!
//! // 在作用域内设置 trace context
//! with_trace_context(ctx, || {
//!     // 在此范围内的所有日志都会携带 trace_id
//!     tracing::info!("Processing order");
//! });
//! ```

use std::cell::RefCell;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// 全局 trace ID 计数器（用于生成唯一ID）
static TRACE_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Trace Context - 请求追踪上下文
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TraceContext {
    /// Trace ID - 全局唯一的追踪标识
    trace_id: String,

    /// Span ID - 当前操作的唯一标识（可选，用于分布式追踪）
    span_id: Option<String>,

    /// Parent Span ID - 父操作的标识（可选）
    parent_span_id: Option<String>,

    /// User ID - 关联的用户ID（可选）
    user_id: Option<u64>,

    /// Request ID - 请求级别的标识（可选）
    request_id: Option<String>,
}

impl TraceContext {
    /// 创建新的 Trace Context
    ///
    /// 自动生成唯一的 trace_id
    #[must_use]
    pub fn new() -> Self {
        Self {
            trace_id: Self::generate_trace_id(),
            span_id: None,
            parent_span_id: None,
            user_id: None,
            request_id: None,
        }
    }

    /// 从 trace_id 字符串创建
    #[must_use]
    pub fn from_trace_id(trace_id: impl Into<String>) -> Self {
        Self {
            trace_id: trace_id.into(),
            span_id: None,
            parent_span_id: None,
            user_id: None,
            request_id: None,
        }
    }

    /// 生成唯一的 Trace ID
    ///
    /// 格式: {timestamp_ms}-{counter}-{random}
    /// 例如: 1699123456789-42-abc123
    fn generate_trace_id() -> String {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);

        let counter = TRACE_ID_COUNTER.fetch_add(1, Ordering::Relaxed);

        // 使用进程ID和线程ID增加唯一性
        let thread_id = std::thread::current().id();
        let thread_hash = format!("{:?}", thread_id)
            .chars()
            .filter(|c| c.is_ascii_hexdigit())
            .take(6)
            .collect::<String>();

        format!("{}-{}-{}", timestamp, counter, thread_hash)
    }

    /// 获取 trace_id
    #[must_use]
    pub fn trace_id(&self) -> &str {
        &self.trace_id
    }

    /// 设置 span_id
    #[must_use]
    pub fn with_span_id(mut self, span_id: impl Into<String>) -> Self {
        self.span_id = Some(span_id.into());
        self
    }

    /// 设置 parent_span_id
    #[must_use]
    pub fn with_parent_span_id(mut self, parent_span_id: impl Into<String>) -> Self {
        self.parent_span_id = Some(parent_span_id.into());
        self
    }

    /// 设置 user_id
    #[must_use]
    pub fn with_user_id(mut self, user_id: u64) -> Self {
        self.user_id = Some(user_id);
        self
    }

    /// 设置 request_id
    #[must_use]
    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }

    /// 获取 span_id
    #[must_use]
    pub fn span_id(&self) -> Option<&str> {
        self.span_id.as_deref()
    }

    /// 获取 user_id
    #[must_use]
    pub fn user_id(&self) -> Option<u64> {
        self.user_id
    }

    /// 获取 request_id
    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        self.request_id.as_deref()
    }

    /// 转换为日志格式的字符串
    ///
    /// 包含所有可用的追踪信息
    #[must_use]
    pub fn to_log_string(&self) -> String {
        let mut parts = vec![format!("trace_id={}", self.trace_id)];

        if let Some(span_id) = &self.span_id {
            parts.push(format!("span_id={}", span_id));
        }

        if let Some(user_id) = self.user_id {
            parts.push(format!("user_id={}", user_id));
        }

        if let Some(request_id) = &self.request_id {
            parts.push(format!("request_id={}", request_id));
        }

        parts.join(" ")
    }
}

impl Default for TraceContext {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for TraceContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_log_string())
    }
}

// ============================================================================
// Thread-Local Storage 支持
// ============================================================================

thread_local! {
    /// 线程本地的 Trace Context
    static CURRENT_TRACE_CONTEXT: RefCell<Option<TraceContext>> = const { RefCell::new(None) };
}

/// 获取当前线程的 Trace Context
///
/// 如果未设置，返回 None
#[must_use]
pub fn current_trace_context() -> Option<TraceContext> {
    CURRENT_TRACE_CONTEXT.with(|ctx| ctx.borrow().clone())
}

/// 设置当前线程的 Trace Context
pub fn set_trace_context(ctx: TraceContext) {
    CURRENT_TRACE_CONTEXT.with(|current| {
        *current.borrow_mut() = Some(ctx);
    });
}

/// 清除当前线程的 Trace Context
pub fn clear_trace_context() {
    CURRENT_TRACE_CONTEXT.with(|current| {
        *current.borrow_mut() = None;
    });
}

/// 在指定的 Trace Context 中执行闭包
///
/// # 示例
/// ```
/// use matching_engine::shared::trace_context::{TraceContext, with_trace_context};
///
/// let ctx = TraceContext::new().with_user_id(123);
/// with_trace_context(ctx, || {
///     // 在此范围内的操作都会关联到这个 trace context
///     println!("Processing with trace context");
/// });
/// ```
pub fn with_trace_context<F, R>(ctx: TraceContext, f: F) -> R
where
    F: FnOnce() -> R,
{
    // 保存旧的上下文
    let old_ctx = current_trace_context();

    // 设置新上下文
    set_trace_context(ctx);

    // 执行闭包
    let result = f();

    // 恢复旧上下文
    if let Some(old) = old_ctx {
        set_trace_context(old);
    } else {
        clear_trace_context();
    }

    result
}

/// Tracing Span 扩展 - 添加 trace context 字段到日志
///
/// 使用此宏可以方便地在 tracing span 中添加 trace context
#[macro_export]
macro_rules! trace_span {
    ($level:expr, $name:expr) => {{
        if let Some(ctx) = $crate::shared::trace_context::current_trace_context() {
            tracing::span!(
                $level,
                $name,
                trace_id = %ctx.trace_id(),
                user_id = ?ctx.user_id()
            )
        } else {
            tracing::span!($level, $name)
        }
    }};
}

/// 日志宏扩展 - 自动包含 trace context
///
/// 在日志中自动包含当前的 trace_id
#[macro_export]
macro_rules! log_with_trace {
    ($level:expr, $($arg:tt)*) => {{
        if let Some(ctx) = $crate::shared::trace_context::current_trace_context() {
            match $level {
                tracing::Level::ERROR => tracing::error!(trace_id = %ctx.trace_id(), $($arg)*),
                tracing::Level::WARN => tracing::warn!(trace_id = %ctx.trace_id(), $($arg)*),
                tracing::Level::INFO => tracing::info!(trace_id = %ctx.trace_id(), $($arg)*),
                tracing::Level::DEBUG => tracing::debug!(trace_id = %ctx.trace_id(), $($arg)*),
                tracing::Level::TRACE => tracing::trace!(trace_id = %ctx.trace_id(), $($arg)*),
            }
        } else {
            match $level {
                tracing::Level::ERROR => tracing::error!($($arg)*),
                tracing::Level::WARN => tracing::warn!($($arg)*),
                tracing::Level::INFO => tracing::info!($($arg)*),
                tracing::Level::DEBUG => tracing::debug!($($arg)*),
                tracing::Level::TRACE => tracing::trace!($($arg)*),
            }
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trace_context_creation() {
        let ctx = TraceContext::new();
        assert!(!ctx.trace_id().is_empty());
        assert!(ctx.span_id().is_none());
        assert!(ctx.user_id().is_none());
    }

    #[test]
    fn test_trace_context_builder() {
        let ctx = TraceContext::new()
            .with_user_id(123)
            .with_span_id("span-1")
            .with_request_id("req-456");

        assert_eq!(ctx.user_id(), Some(123));
        assert_eq!(ctx.span_id(), Some("span-1"));
        assert_eq!(ctx.request_id(), Some("req-456"));
    }

    #[test]
    fn test_trace_id_uniqueness() {
        let ctx1 = TraceContext::new();
        let ctx2 = TraceContext::new();

        assert_ne!(ctx1.trace_id(), ctx2.trace_id());
    }

    #[test]
    fn test_thread_local_context() {
        // 初始应该为 None
        assert!(current_trace_context().is_none());

        // 设置 context
        let ctx = TraceContext::new().with_user_id(999);
        set_trace_context(ctx.clone());

        // 应该能获取到
        let retrieved = current_trace_context();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().user_id(), Some(999));

        // 清除后应该为 None
        clear_trace_context();
        assert!(current_trace_context().is_none());
    }

    #[test]
    fn test_with_trace_context() {
        let ctx = TraceContext::new().with_user_id(777);

        let result = with_trace_context(ctx.clone(), || {
            // 在此范围内应该能获取到 context
            let current = current_trace_context();
            assert!(current.is_some());
            assert_eq!(current.unwrap().user_id(), Some(777));
            42
        });

        assert_eq!(result, 42);

        // 作用域外应该已清除（如果之前没有 context）
        assert!(current_trace_context().is_none());
    }

    #[test]
    fn test_to_log_string() {
        let ctx = TraceContext::new()
            .with_user_id(123)
            .with_span_id("span-1")
            .with_request_id("req-456");

        let log_str = ctx.to_log_string();
        assert!(log_str.contains("trace_id="));
        assert!(log_str.contains("user_id=123"));
        assert!(log_str.contains("span_id=span-1"));
        assert!(log_str.contains("request_id=req-456"));
    }

    #[test]
    fn test_display_trait() {
        let ctx = TraceContext::new().with_user_id(999);
        let display = format!("{}", ctx);
        assert!(display.contains("trace_id="));
        assert!(display.contains("user_id=999"));
    }
}
