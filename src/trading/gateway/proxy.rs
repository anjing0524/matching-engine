//! Standalone gateway relay: member TCP connections are forwarded frame-for-frame
//! to the middleware gateway listener.

use std::io;
#[cfg(not(target_os = "linux"))]
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
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
        let _ = relay_stream(&mut reader, &mut writer);
        let _ = writer.shutdown(Shutdown::Both);
    });
    let mut reader = member_read;
    let mut writer = upstream_write;
    let _ = relay_stream(&mut reader, &mut writer);
    let _ = writer.shutdown(Shutdown::Both);
    let _ = upstream_thread.join();
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn relay_stream(reader: &mut TcpStream, writer: &mut TcpStream) -> io::Result<()> {
    use std::os::raw::{c_int, c_uint};

    unsafe extern "C" {
        fn pipe(fds: *mut c_int) -> c_int;
        fn splice(
            fd_in: c_int,
            off_in: *mut i64,
            fd_out: c_int,
            off_out: *mut i64,
            len: usize,
            flags: c_uint,
        ) -> isize;
    }

    let mut fds = [0i32; 2];
    // SAFETY: fds points to two valid c_int slots; pipe writes two descriptors.
    if unsafe { pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: pipe returned two fresh descriptors owned by this function.
    let read_fd = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    // SAFETY: same as above for the write end.
    let write_fd = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    let input = reader.as_raw_fd();
    let output = writer.as_raw_fd();
    loop {
        // SAFETY: descriptors are live for the duration of the call; null offsets
        // request sequential socket reads.
        let read = unsafe {
            splice(
                input,
                std::ptr::null_mut(),
                write_fd.as_raw_fd(),
                std::ptr::null_mut(),
                64 * 1024,
                1,
            )
        };
        if read == 0 {
            return Ok(());
        }
        if read < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut moved = 0isize;
        while moved < read {
            // SAFETY: descriptors are live; the pipe contains exactly `read`
            // bytes and output is a blocking socket.
            let written = unsafe {
                splice(
                    read_fd.as_raw_fd(),
                    std::ptr::null_mut(),
                    output,
                    std::ptr::null_mut(),
                    (read - moved) as usize,
                    1,
                )
            };
            if written < 0 {
                return Err(io::Error::last_os_error());
            }
            moved += written;
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn relay_stream(reader: &mut TcpStream, writer: &mut TcpStream) -> io::Result<()> {
    pump(reader, writer)
}

#[cfg(not(target_os = "linux"))]
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
