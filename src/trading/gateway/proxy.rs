//! Standalone gateway relay: member TCP connections are forwarded frame-for-frame
//! to the middleware gateway listener.

use std::io;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

pub struct GatewayProxyHandle {
    addr: std::net::SocketAddr,
    shutdown: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

impl GatewayProxyHandle {
    #[must_use]
    pub fn addr(&self) -> std::net::SocketAddr {
        self.addr
    }

    pub fn stop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for GatewayProxyHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Starts a TCP relay from members to one middleware listener.
///
/// # Errors
/// Returns an I/O error if the listener cannot bind.
pub fn start(listen: &str, upstream: String) -> io::Result<GatewayProxyHandle> {
    let listener = TcpListener::bind(listen)?;
    listener.set_nonblocking(true)?;
    let addr = listener.local_addr()?;
    let shutdown = Arc::new(AtomicBool::new(false));
    let accept_shutdown = Arc::clone(&shutdown);
    let join = thread::Builder::new()
        .name("trading-gateway-proxy".to_string())
        .spawn(move || {
            while !accept_shutdown.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((member, _)) => {
                        let upstream = upstream.clone();
                        let _ = thread::Builder::new()
                            .name("trading-gateway-proxy-conn".to_string())
                            .spawn(move || relay(member, upstream));
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        })?;
    Ok(GatewayProxyHandle {
        addr,
        shutdown,
        join: Some(join),
    })
}

fn relay(member: TcpStream, upstream_addr: String) {
    let Ok(upstream) = TcpStream::connect(upstream_addr) else {
        return;
    };
    let _ = member.set_nonblocking(false);
    let _ = upstream.set_nonblocking(false);
    let _ = member.set_nodelay(true);
    let _ = upstream.set_nodelay(true);
    let Ok(member_read) = member.try_clone() else {
        return;
    };
    let Ok(upstream_read) = upstream.try_clone() else {
        return;
    };
    let member_write = member;
    let upstream_write = upstream;
    let upstream_thread = thread::spawn(move || {
        let mut reader = upstream_read;
        let mut writer = member_write;
        let _ = pump(&mut reader, &mut writer);
        let _ = writer.shutdown(Shutdown::Both);
    });
    let mut reader = member_read;
    let mut writer = upstream_write;
    let _ = pump(&mut reader, &mut writer);
    let _ = writer.shutdown(Shutdown::Both);
    let _ = upstream_thread.join();
}

fn pump(reader: &mut TcpStream, writer: &mut TcpStream) -> io::Result<()> {
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok(());
        }
        writer.write_all(&buffer[..read])?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn relays_bytes_to_upstream() {
        let echo = TcpListener::bind("127.0.0.1:0").unwrap();
        let echo_addr = echo.local_addr().unwrap();
        thread::spawn(move || {
            if let Ok((mut stream, _)) = echo.accept() {
                let mut buffer = [0u8; 4];
                if stream.read_exact(&mut buffer).is_ok() {
                    let _ = stream.write_all(&buffer);
                }
            }
        });

        let mut proxy = start("127.0.0.1:0", echo_addr.to_string()).unwrap();
        let mut member = TcpStream::connect(proxy.addr()).unwrap();
        member.write_all(b"ping").unwrap();
        let mut response = [0u8; 4];
        member.read_exact(&mut response).unwrap();
        assert_eq!(&response, b"ping");
        proxy.stop();
    }
}
