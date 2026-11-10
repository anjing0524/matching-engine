//! Minimal HTTP metrics endpoint for `/metrics`.

use crate::trading::market_data::metrics::TradingMetrics;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub struct MetricsServerHandle {
    addr: std::net::SocketAddr,
    shutdown: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

impl MetricsServerHandle {
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

impl Drop for MetricsServerHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Starts the metrics HTTP listener.
///
/// # Errors
/// Returns an I/O error if the listener cannot bind.
pub fn start(
    bind_addr: &str,
    metrics: Arc<Mutex<TradingMetrics>>,
) -> io::Result<MetricsServerHandle> {
    let listener = TcpListener::bind(bind_addr)?;
    listener.set_nonblocking(true)?;
    let addr = listener.local_addr()?;
    let shutdown = Arc::new(AtomicBool::new(false));
    let accept_shutdown = Arc::clone(&shutdown);
    let join = thread::Builder::new()
        .name("trading-metrics-http".to_string())
        .spawn(move || {
            while !accept_shutdown.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let metrics = Arc::clone(&metrics);
                        let _ = thread::Builder::new()
                            .name("trading-metrics-conn".to_string())
                            .spawn(move || serve(stream, &metrics));
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        })?;
    Ok(MetricsServerHandle {
        addr,
        shutdown,
        join: Some(join),
    })
}

fn serve(mut stream: TcpStream, metrics: &Arc<Mutex<TradingMetrics>>) {
    let _ = stream.set_nonblocking(false);
    let mut request = [0u8; 1024];
    let read = stream.read(&mut request).unwrap_or(0);
    let first_line = String::from_utf8_lossy(&request[..read]);
    if !first_line.starts_with("GET /metrics ") {
        let body = "not found\n";
        let response = format!(
            "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes());
        return;
    }

    let body = metrics
        .lock()
        .map_or_else(|_| String::new(), |metrics| metrics.to_prometheus());
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpStream;

    #[test]
    fn serves_metrics_text() {
        let metrics = Arc::new(Mutex::new(TradingMetrics {
            commands_executed: 9,
            ..TradingMetrics::default()
        }));
        let mut server = start("127.0.0.1:0", Arc::clone(&metrics)).unwrap();
        let mut stream = TcpStream::connect(server.addr()).unwrap();
        stream
            .write_all(b"GET /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("trading_commands_total 9\n"));
        server.stop();
    }
}
