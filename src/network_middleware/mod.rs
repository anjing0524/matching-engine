/// 高性能网络中间件模块
///
/// 提供统一的网络抽象层，支持多种后端：
/// - Tokio: 异步I/O (开发/测试)
/// - DPDK: 零拷贝用户态网络栈 (生产环境)
/// - FPGA: 硬件加速 (超高频场景)

pub mod traits;
pub mod buffer;
pub mod codec;
pub mod tokio_backend;
pub mod metrics;

#[cfg(feature = "dpdk")]
pub mod dpdk_backend;

#[cfg(feature = "fpga")]
pub mod fpga_backend;

pub use traits::{NetworkTransport, ZeroCopyBuffer, Connection};
pub use buffer::SharedBuffer;
pub use codec::{Codec, BincodeCodec, LengthDelimitedCodec};
pub use metrics::NetworkMetrics;

use std::net::SocketAddr;
use std::sync::Arc;

/// 网络后端类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendType {
    /// Tokio异步I/O (默认)
    Tokio,
    /// DPDK用户态网络栈
    Dpdk,
    /// FPGA硬件加速
    Fpga,
}

/// 网络中间件配置
#[derive(Debug, Clone)]
pub struct MiddlewareConfig {
    /// 后端类型
    pub backend: BackendType,

    /// 监听地址
    pub listen_addr: SocketAddr,

    /// 零拷贝缓冲区大小
    pub buffer_size: usize,

    /// 接收队列深度
    pub rx_queue_depth: usize,

    /// 发送队列深度
    pub tx_queue_depth: usize,

    /// CPU核心绑定（可选）
    pub cpu_affinity: Option<Vec<usize>>,
}

impl Default for MiddlewareConfig {
    fn default() -> Self {
        Self {
            backend: BackendType::Tokio,
            listen_addr: "127.0.0.1:8080".parse().unwrap(),
            buffer_size: 65536,
            rx_queue_depth: 1024,
            tx_queue_depth: 1024,
            cpu_affinity: None,
        }
    }
}

/// 网络中间件
///
/// 统一的网络抽象层，屏蔽底层传输细节
pub struct NetworkMiddleware<C: Codec> {
    /// 网络传输后端
    backend: Box<dyn NetworkTransport>,

    /// 编解码器
    codec: C,

    /// 性能指标
    metrics: Arc<NetworkMetrics>,

    /// 配置
    config: MiddlewareConfig,
}

impl<C: Codec> NetworkMiddleware<C> {
    /// 创建网络中间件
    pub fn new(config: MiddlewareConfig, codec: C) -> Result<Self, MiddlewareError> {
        let backend = match config.backend {
            BackendType::Tokio => {
                Box::new(tokio_backend::TokioTransport::new()?) as Box<dyn NetworkTransport>
            }
            #[cfg(feature = "dpdk")]
            BackendType::Dpdk => {
                Box::new(dpdk_backend::DpdkTransport::new()?) as Box<dyn NetworkTransport>
            }
            #[cfg(feature = "fpga")]
            BackendType::Fpga => {
                Box::new(fpga_backend::FpgaTransport::new()?) as Box<dyn NetworkTransport>
            }
            _ => {
                return Err(MiddlewareError::UnsupportedBackend(config.backend));
            }
        };

        Ok(Self {
            backend,
            codec,
            metrics: Arc::new(NetworkMetrics::new()),
            config,
        })
    }

    /// 启动服务
    pub async fn serve(&mut self) -> Result<(), MiddlewareError> {
        self.backend.bind(self.config.listen_addr).await?;

        loop {
            // 接受新连接
            let conn = self.backend.accept().await?;
            let metrics = Arc::clone(&self.metrics);

            // 为每个连接spawn任务
            tokio::spawn(async move {
                if let Err(e) = Self::handle_connection(conn, metrics).await {
                    eprintln!("Connection error: {}", e);
                }
            });
        }
    }

    /// 处理连接
    async fn handle_connection(
        mut conn: Box<dyn Connection>,
        metrics: Arc<NetworkMetrics>,
    ) -> Result<(), MiddlewareError> {
        loop {
            // 零拷贝接收
            let buf = conn.recv().await?;

            // 更新指标
            metrics.record_rx_packet(buf.len());

            // TODO: 解码并处理消息
        }
    }

    /// 获取性能指标
    pub fn metrics(&self) -> Arc<NetworkMetrics> {
        Arc::clone(&self.metrics)
    }
}

/// 中间件错误类型
#[derive(Debug, thiserror::Error)]
pub enum MiddlewareError {
    #[error("Unsupported backend: {0:?}")]
    UnsupportedBackend(BackendType),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Backend error: {0}")]
    Backend(String),

    #[error("Codec error: {0}")]
    Codec(String),
}
