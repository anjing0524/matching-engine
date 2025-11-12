/// Tokio网络后端实现（基线）

use crate::infrastructure::network::buffer::SharedBuffer;
use crate::infrastructure::network::traits::{Connection, NetworkTransport, ZeroCopyBuffer};
use async_trait::async_trait;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Tokio TCP连接
pub struct TokioConnection {
    id: u64,
    stream: TcpStream,
    peer_addr: SocketAddr,
}

impl TokioConnection {
    fn new(id: u64, stream: TcpStream, peer_addr: SocketAddr) -> Self {
        Self {
            id,
            stream,
            peer_addr,
        }
    }
}

#[async_trait]
impl Connection for TokioConnection {
    async fn recv(&mut self) -> std::io::Result<Box<dyn ZeroCopyBuffer>> {
        // 先读取4字节长度前缀
        let len = self.stream.read_u32().await? as usize;

        // 分配缓冲区
        let mut data = vec![0u8; len];
        self.stream.read_exact(&mut data).await?;

        Ok(Box::new(SharedBuffer::from_vec(data)))
    }

    async fn send(&mut self, buf: Box<dyn ZeroCopyBuffer>) -> std::io::Result<()> {
        // 写入长度前缀
        let len = buf.len() as u32;
        self.stream.write_u32(len).await?;

        // 写入数据
        self.stream.write_all(buf.as_slice()).await?;
        self.stream.flush().await?;

        Ok(())
    }

    fn peer_addr(&self) -> std::io::Result<SocketAddr> {
        Ok(self.peer_addr)
    }

    fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.stream.local_addr()
    }
}

/// Tokio TCP传输
pub struct TokioTransport {
    listener: Option<TcpListener>,
    next_conn_id: Arc<AtomicU64>,
}

impl TokioTransport {
    pub fn new() -> std::io::Result<Self> {
        Ok(Self {
            listener: None,
            next_conn_id: Arc::new(AtomicU64::new(1)),
        })
    }
}

#[async_trait]
impl NetworkTransport for TokioTransport {
    async fn bind(&mut self, addr: SocketAddr) -> std::io::Result<()> {
        let listener = TcpListener::bind(addr).await?;
        self.listener = Some(listener);
        Ok(())
    }

    async fn accept(&mut self) -> std::io::Result<Box<dyn Connection>> {
        let listener = self
            .listener
            .as_ref()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "Not bound"))?;

        let (stream, peer_addr) = listener.accept().await?;

        let conn_id = self.next_conn_id.fetch_add(1, Ordering::Relaxed);

        Ok(Box::new(TokioConnection::new(conn_id, stream, peer_addr)))
    }

    fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener
            .as_ref()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "Not bound"))?
            .local_addr()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_tokio_transport() {
        let mut transport = TokioTransport::new().unwrap();

        // 绑定
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        transport.bind(addr).await.unwrap();

        let local_addr = transport.local_addr().unwrap();
        println!("Listening on {}", local_addr);

        // 在后台启动客户端连接
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            let _stream = TcpStream::connect(local_addr).await.unwrap();
        });

        // 接受连接
        let conn = transport.accept().await.unwrap();
        println!("Accepted connection from {}", conn.peer_addr().unwrap());
    }
}
