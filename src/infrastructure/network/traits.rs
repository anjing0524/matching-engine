/// 网络中间件核心trait定义

use async_trait::async_trait;
use std::net::SocketAddr;
use std::sync::Arc;

/// 零拷贝缓冲区trait
///
/// 定义了统一的零拷贝缓冲区接口，支持多种后端实现
pub trait ZeroCopyBuffer: Send + Sync {
    /// 获取只读数据切片
    fn as_slice(&self) -> &[u8];

    /// 获取可写数据切片
    fn as_mut_slice(&mut self) -> &mut [u8];

    /// 缓冲区长度
    fn len(&self) -> usize {
        self.as_slice().len()
    }

    /// 是否为空
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// DMA物理地址（用于DPDK/FPGA）
    ///
    /// 如果后端不支持DMA，返回None
    fn dma_addr(&self) -> Option<u64> {
        None
    }

    /// 零拷贝克隆（引用计数）
    ///
    /// 创建一个新的缓冲区引用，共享底层内存
    fn clone_ref(&self) -> Arc<dyn ZeroCopyBuffer>;
}

/// 网络连接trait
///
/// 表示一个客户端连接，支持零拷贝收发
#[async_trait]
pub trait Connection: Send {
    /// 接收数据（零拷贝）
    ///
    /// 返回接收到的缓冲区，缓冲区可能来自：
    /// - 堆分配内存 (Tokio)
    /// - DMA内存池 (DPDK)
    /// - FPGA DMA ring (FPGA)
    async fn recv(&mut self) -> std::io::Result<Box<dyn ZeroCopyBuffer>>;

    /// 发送数据（零拷贝）
    ///
    /// 接受任何实现了ZeroCopyBuffer的缓冲区
    async fn send(&mut self, buf: Box<dyn ZeroCopyBuffer>) -> std::io::Result<()>;

    /// 对端地址
    fn peer_addr(&self) -> std::io::Result<SocketAddr>;

    /// 本地地址
    fn local_addr(&self) -> std::io::Result<SocketAddr>;
}

/// 网络传输trait
///
/// 定义了网络后端的统一接口
#[async_trait]
pub trait NetworkTransport: Send {
    /// 绑定并监听地址
    async fn bind(&mut self, addr: SocketAddr) -> std::io::Result<()>;

    /// 接受新连接
    ///
    /// 阻塞直到有新连接到达
    async fn accept(&mut self) -> std::io::Result<Box<dyn Connection>>;

    /// 获取本地监听地址
    fn local_addr(&self) -> std::io::Result<SocketAddr>;
}

/// 批量I/O trait（可选优化）
///
/// 某些后端（DPDK/FPGA）支持批量收发以提升性能
#[async_trait]
pub trait BatchedTransport: NetworkTransport {
    /// 批量接收
    ///
    /// 一次接收多个数据包，减少系统调用开销
    async fn recv_batch(
        &mut self,
        max_batch_size: usize,
    ) -> std::io::Result<Vec<(u64, Box<dyn ZeroCopyBuffer>)>>;

    /// 批量发送
    ///
    /// 一次发送多个数据包，减少系统调用开销
    async fn send_batch(
        &mut self,
        buffers: Vec<(u64, Box<dyn ZeroCopyBuffer>)>,
    ) -> std::io::Result<()>;
}

/// 零拷贝内存池trait
///
/// 用于预分配和复用缓冲区
pub trait BufferPool: Send + Sync {
    /// 从池中分配缓冲区
    fn alloc(&self, size: usize) -> std::io::Result<Box<dyn ZeroCopyBuffer>>;

    /// 归还缓冲区到池
    fn free(&self, buf: Box<dyn ZeroCopyBuffer>);

    /// 池中可用缓冲区数量
    fn available(&self) -> usize;

    /// 池的总容量
    fn capacity(&self) -> usize;
}

/// DMA缓冲区trait（DPDK/FPGA专用）
pub trait DmaBuffer: ZeroCopyBuffer {
    /// 获取DMA物理地址（必须实现）
    fn physical_addr(&self) -> u64;

    /// 获取IOVA地址（I/O虚拟地址）
    fn iova_addr(&self) -> u64;

    /// 是否来自大页内存
    fn is_hugepage(&self) -> bool;
}
