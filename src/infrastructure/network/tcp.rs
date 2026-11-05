//! Blocking TCP transport.
//!
//! A matching gateway serves a bounded number of client sessions, so one
//! dedicated OS thread per connection is simpler and more predictable than an
//! async runtime: no task scheduler, no future polling, no per-message waker.
//! The matching core is always a separate single-writer thread; this layer only
//! moves length-prefixed frames.
//!
//! For very large connection counts (thousands+) an epoll/io_uring event loop
//! would be required; the thread-per-connection tradeoff is documented in the
//! architecture plan.
use crate::shared::wire::MAX_FRAME_LEN;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

/// A blocking TCP connection carrying length-prefixed wire frames.
pub struct TcpConnection {
    stream: TcpStream,
    peer_addr: SocketAddr,
    /// Reused receive buffer: no per-message allocation in steady state.
    recv_buf: Vec<u8>,
}

impl TcpConnection {
    /// Wraps an accepted stream.
    #[must_use]
    pub fn new(stream: TcpStream, peer_addr: SocketAddr) -> Self {
        Self {
            stream,
            peer_addr,
            recv_buf: Vec::new(),
        }
    }

    /// Receives one frame payload into a reusable buffer.
    ///
    /// A read timeout surfaces as `WouldBlock`/`TimedOut`; callers treat it as
    /// an idle disconnect or a shutdown wakeup.
    pub fn recv(&mut self) -> std::io::Result<&[u8]> {
        let mut len_buf = [0u8; 4];
        self.stream.read_exact(&mut len_buf)?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if len > MAX_FRAME_LEN {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("frame length {len} exceeds maximum {MAX_FRAME_LEN}"),
            ));
        }

        self.recv_buf.resize(len, 0);
        self.stream.read_exact(&mut self.recv_buf)?;
        Ok(&self.recv_buf)
    }

    /// Sends one frame: a big-endian `u32` length prefix followed by `payload`.
    pub fn send(&mut self, payload: &[u8]) -> std::io::Result<()> {
        if payload.len() > MAX_FRAME_LEN {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "frame length {} exceeds maximum {MAX_FRAME_LEN}",
                    payload.len()
                ),
            ));
        }
        let len = u32::try_from(payload.len()).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("frame length {} exceeds u32::MAX", payload.len()),
            )
        })?;

        self.stream.write_all(&len.to_be_bytes())?;
        self.stream.write_all(payload)?;
        self.stream.flush()
    }

    /// Sets the socket's read timeout.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        self.stream.set_read_timeout(timeout)
    }

    /// Sets the socket's write timeout.
    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        self.stream.set_write_timeout(timeout)
    }

    /// Disables Nagle's algorithm.
    pub fn set_nodelay(&self, enabled: bool) -> std::io::Result<()> {
        self.stream.set_nodelay(enabled)
    }

    /// Returns the peer address.
    #[must_use]
    pub fn peer_addr(&self) -> SocketAddr {
        self.peer_addr
    }

    /// Returns the local address.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.stream.local_addr()
    }

    /// Shuts down both directions of the socket.
    pub fn shutdown(&self) -> std::io::Result<()> {
        self.stream.shutdown(Shutdown::Both)
    }

    /// Clones the underlying stream (used by the shutdown registry).
    pub fn try_clone(&self) -> std::io::Result<TcpStream> {
        self.stream.try_clone()
    }
}

/// A blocking TCP listener.
#[derive(Default)]
pub struct TcpTransport {
    listener: Option<TcpListener>,
}

impl TcpTransport {
    /// Creates an unbound transport.
    pub fn new() -> std::io::Result<Self> {
        Ok(Self { listener: None })
    }

    /// Binds to `addr`, starts listening, and enables nonblocking accept.
    pub fn bind(&mut self, addr: SocketAddr) -> std::io::Result<()> {
        let listener = TcpListener::bind(addr)?;
        listener.set_nonblocking(true)?;
        self.listener = Some(listener);
        Ok(())
    }

    /// Accepts the next connection or returns `WouldBlock` when none is ready.
    pub fn accept(&mut self) -> std::io::Result<TcpConnection> {
        let listener = self
            .listener
            .as_ref()
            .ok_or_else(|| std::io::Error::other("transport is not bound"))?;
        let (stream, peer_addr) = listener.accept()?;
        // The listener is nonblocking for shutdown polling; accepted sockets
        // must be blocking for the per-connection thread.
        stream.set_nonblocking(false)?;
        // Trading traffic is small request/response frames; Nagle would batch
        // them and add up to tens of milliseconds of latency.
        stream.set_nodelay(true)?;
        Ok(TcpConnection::new(stream, peer_addr))
    }

    /// Returns the bound local address.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener
            .as_ref()
            .ok_or_else(|| std::io::Error::other("transport is not bound"))?
            .local_addr()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_transport_roundtrip() {
        let mut transport = TcpTransport::new().expect("failed to create transport");
        transport
            .bind("127.0.0.1:0".parse().expect("invalid address"))
            .expect("failed to bind");
        let address = transport.local_addr().expect("no local address");

        let client = thread::spawn(move || {
            let mut stream = TcpStream::connect(address).expect("connect failed");
            stream.write_all(&3u32.to_be_bytes()).unwrap();
            stream.write_all(b"abc").unwrap();
            let mut len = [0u8; 4];
            stream.read_exact(&mut len).unwrap();
            let mut reply = vec![0u8; u32::from_be_bytes(len) as usize];
            stream.read_exact(&mut reply).unwrap();
            assert_eq!(&reply, b"pong");
        });

        let mut connection = loop {
            match transport.accept() {
                Ok(connection) => break connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("accept failed: {error}"),
            }
        };
        let payload = connection.recv().expect("recv failed");
        assert_eq!(payload, b"abc");
        connection.send(b"pong").expect("send failed");
        client.join().expect("client failed");
    }
}
