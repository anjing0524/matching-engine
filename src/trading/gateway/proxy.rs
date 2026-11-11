//! Standalone gateway relay: member TCP connections are forwarded frame-for-frame
//! to the middleware gateway listener.

use std::io::{self, Read, Write};
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

struct Relay {
    member: TcpStream,
    upstream: TcpStream,
    to_upstream: Vec<u8>,
    upstream_offset: usize,
    to_member: Vec<u8>,
    member_offset: usize,
}

impl Relay {
    fn new(member: TcpStream, upstream: TcpStream) -> io::Result<Self> {
        member.set_nonblocking(true)?;
        upstream.set_nonblocking(true)?;
        member.set_nodelay(true)?;
        upstream.set_nodelay(true)?;
        Ok(Self {
            member,
            upstream,
            to_upstream: Vec::with_capacity(64 * 1024),
            upstream_offset: 0,
            to_member: Vec::with_capacity(64 * 1024),
            member_offset: 0,
        })
    }

    fn pump(&mut self) -> bool {
        // The two directions are independent: a blocked member read does not
        // prevent an upstream response from being drained.
        if !relay_direction(
            &mut self.member,
            &mut self.upstream,
            &mut self.to_upstream,
            &mut self.upstream_offset,
        ) {
            return false;
        }
        relay_direction(
            &mut self.upstream,
            &mut self.member,
            &mut self.to_member,
            &mut self.member_offset,
        )
    }
}

fn relay_direction(
    reader: &mut TcpStream,
    writer: &mut TcpStream,
    buffer: &mut Vec<u8>,
    offset: &mut usize,
) -> bool {
    if *offset == buffer.len() {
        buffer.clear();
        *offset = 0;
    }
    if buffer.is_empty() {
        let mut chunk = [0u8; 64 * 1024];
        match reader.read(&mut chunk) {
            Ok(0) => return false,
            Ok(count) => buffer.extend_from_slice(&chunk[..count]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return true,
            Err(_) => return false,
        }
    }
    while *offset < buffer.len() {
        match writer.write(&buffer[*offset..]) {
            Ok(0) => return false,
            Ok(count) => *offset += count,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return true,
            Err(_) => return false,
        }
    }
    buffer.clear();
    *offset = 0;
    true
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
            let mut relays = Vec::new();
            while !accept_shutdown.load(Ordering::Acquire) {
                loop {
                    match listener.accept() {
                        Ok((member, _)) => {
                            let Ok(upstream) = TcpStream::connect(&upstream) else {
                                continue;
                            };
                            let Ok(relay) = Relay::new(member, upstream) else {
                                continue;
                            };
                            relays.push(relay);
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            break;
                        }
                        Err(_) => return,
                    }
                }
                let mut index = 0;
                while index < relays.len() {
                    if relays[index].pump() {
                        index += 1;
                    } else {
                        let mut relay = relays.swap_remove(index);
                        let _ = relay.member.shutdown(Shutdown::Both);
                        let _ = relay.upstream.shutdown(Shutdown::Both);
                    }
                }
                std::hint::spin_loop();
            }
        })?;
    Ok(GatewayProxyHandle {
        addr,
        shutdown,
        join: Some(join),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
