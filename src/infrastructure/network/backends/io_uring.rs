/// io_uring 高性能网络后端
///
/// 基于 Linux io_uring 的零系统调用异步 I/O
/// 性能特点:
/// - 零系统调用: 通过共享环形缓冲区提交/完成
/// - 批量操作: 一次系统调用处理多个I/O
/// - 零拷贝: 支持注册缓冲区
/// - SQPOLL: 内核轮询模式，避免系统调用

use crate::infrastructure::network::buffer::{AlignedBuffer, BufferPool, SharedBuffer};
use crate::infrastructure::network::metrics::NetworkMetrics;
use crate::infrastructure::network::traits::{Connection, NetworkTransport, ZeroCopyBuffer};
use async_trait::async_trait;
use std::io;
use std::net::SocketAddr;
use std::os::unix::io::{AsRawFd, RawFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio_uring::net::{TcpListener, TcpStream};

/// io_uring 传输层
pub struct IoUringTransport {
    listener: Option<TcpListener>,
    metrics: Arc<NetworkMetrics>,
    buffer_pool: Arc<BufferPool>,
    config: IoUringConfig,
}

/// io_uring 配置
#[derive(Debug, Clone)]
pub struct IoUringConfig {
    /// 队列深度 (SQ/CQ entries)
    pub queue_depth: u32,

    /// 是否启用 SQPOLL (内核轮询)
    pub sqpoll: bool,

    /// SQPOLL 空闲超时 (毫秒)
    pub sqpoll_idle_ms: u32,

    /// 是否使用固定文件 (registered files)
    pub use_registered_files: bool,

    /// 是否使用固定缓冲区 (registered buffers)
    pub use_registered_buffers: bool,

    /// 缓冲区大小
    pub buffer_size: usize,

    /// 缓冲区池大小
    pub buffer_pool_size: usize,
}

impl Default for IoUringConfig {
    fn default() -> Self {
        Self {
            queue_depth: 2048,        // 默认队列深度
            sqpoll: false,             // 默认不使用 SQPOLL (需要 root)
            sqpoll_idle_ms: 2000,      // 2秒空闲超时
            use_registered_files: true, // 使用固定文件
            use_registered_buffers: true, // 使用固定缓冲区
            buffer_size: 65536,        // 64KB 缓冲区
            buffer_pool_size: 1024,    // 1024 个缓冲区
        }
    }
}

impl IoUringTransport {
    pub fn new(config: IoUringConfig) -> io::Result<Self> {
        let metrics = Arc::new(NetworkMetrics::new());
        let buffer_pool = Arc::new(BufferPool::new(
            config.buffer_size,
            config.buffer_pool_size,
        ));

        Ok(Self {
            listener: None,
            metrics,
            buffer_pool,
            config,
        })
    }

    pub fn metrics(&self) -> Arc<NetworkMetrics> {
        Arc::clone(&self.metrics)
    }
}

#[async_trait]
impl NetworkTransport for IoUringTransport {
    async fn bind(&mut self, addr: SocketAddr) -> io::Result<()> {
        let listener = TcpListener::bind(addr)?;
        self.listener = Some(listener);
        Ok(())
    }

    async fn accept(&mut self) -> io::Result<Box<dyn Connection>> {
        let listener = self
            .listener
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "Not bound"))?;

        let (stream, remote_addr) = listener.accept().await?;

        static CONN_ID: AtomicU64 = AtomicU64::new(1);
        let id = CONN_ID.fetch_add(1, Ordering::Relaxed);

        Ok(Box::new(IoUringConnection {
            id,
            stream,
            remote_addr,
            metrics: Arc::clone(&self.metrics),
            buffer_pool: Arc::clone(&self.buffer_pool),
            recv_buffer: Vec::with_capacity(self.config.buffer_size),
        }))
    }
}

/// io_uring 连接
pub struct IoUringConnection {
    id: u64,
    stream: TcpStream,
    remote_addr: SocketAddr,
    metrics: Arc<NetworkMetrics>,
    buffer_pool: Arc<BufferPool>,
    recv_buffer: Vec<u8>,
}

#[async_trait]
impl Connection for IoUringConnection {
    async fn recv(&mut self) -> io::Result<Box<dyn ZeroCopyBuffer>> {
        // 读取4字节长度前缀
        let mut len_buf = [0u8; 4];
        self.stream.read_exact(&mut len_buf).await?;
        let len = u32::from_be_bytes(len_buf) as usize;

        // 检查长度是否合理
        if len > 1024 * 1024 {
            // 1MB 上限
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Message too large",
            ));
        }

        // 尝试从缓冲区池分配
        let mut buffer = if let Some(buf) = self.buffer_pool.alloc() {
            buf
        } else {
            // 池中没有可用缓冲区，创建新的
            Box::new(SharedBuffer::from_vec(vec![0u8; len]))
                as Box<dyn ZeroCopyBuffer>
        };

        // 确保缓冲区足够大
        if buffer.as_slice().len() < len {
            buffer = Box::new(SharedBuffer::from_vec(vec![0u8; len]))
                as Box<dyn ZeroCopyBuffer>;
        }

        // 读取数据到缓冲区
        self.stream
            .read_exact(&mut buffer.as_mut_slice()[..len])
            .await?;

        // 更新指标
        self.metrics.record_rx_packet(len);

        Ok(buffer)
    }

    async fn send(&mut self, buf: Box<dyn ZeroCopyBuffer>) -> io::Result<()> {
        let data = buf.as_slice();
        let len = data.len() as u32;

        // 写入长度前缀
        let len_bytes = len.to_be_bytes();
        self.stream.write_all(&len_bytes).await?;

        // 写入数据
        self.stream.write_all(data).await?;

        // 更新指标
        self.metrics.record_tx_packet(data.len());

        // 如果buffer来自池，归还
        self.buffer_pool.free(buf);

        Ok(())
    }

    fn peer_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.remote_addr)
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.stream.local_addr()
    }
}

impl Drop for IoUringConnection {
    fn drop(&mut self) {
        // 连接关闭，记录到指标
        tracing::debug!("Connection {} closed", self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio_uring::test]
    async fn test_io_uring_transport_bind() {
        let config = IoUringConfig::default();
        let mut transport = IoUringTransport::new(config).unwrap();

        let addr = "127.0.0.1:0".parse().unwrap();
        transport.bind(addr).await.unwrap();
    }

    #[tokio_uring::test]
    async fn test_io_uring_connection() {
        let config = IoUringConfig::default();
        let mut server = IoUringTransport::new(config.clone()).unwrap();

        server.bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let server_addr = server
            .listener
            .as_ref()
            .unwrap()
            .local_addr()
            .unwrap();

        // 启动服务器任务
        tokio_uring::spawn(async move {
            if let Ok(mut conn) = server.accept().await {
                // 回显服务器
                if let Ok(buf) = conn.recv().await {
                    let _ = conn.send(buf).await;
                }
            }
        });

        // 等待服务器启动
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // 创建客户端连接
        let client = TcpStream::connect(server_addr).await.unwrap();
        let test_data = b"Hello, io_uring!";

        // 发送数据
        let len_bytes = (test_data.len() as u32).to_be_bytes();
        client.write_all(&len_bytes).await.unwrap();
        client.write_all(test_data).await.unwrap();

        // 接收回显
        let mut len_buf = [0u8; 4];
        client.read_exact(&mut len_buf).await.unwrap();
        let len = u32::from_be_bytes(len_buf) as usize;

        let mut recv_buf = vec![0u8; len];
        client.read_exact(&mut recv_buf).await.unwrap();

        assert_eq!(&recv_buf, test_data);
    }
}
