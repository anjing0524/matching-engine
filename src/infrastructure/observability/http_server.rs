//! HTTP observability server.
//!
//! A tiny blocking HTTP/1.1 server for `/metrics`, `/health`, `/health/ready`,
//! and `/health/live`. It intentionally avoids an async web framework: the
//! matching data path does not need a runtime, and one thread per scrape is
//! sufficient for an internal Prometheus/health endpoint.
use super::health::{HealthChecker, HealthDetails, HealthStatus};
use super::metrics_collector::GLOBAL_METRICS;
use crate::shared::metrics::METRICS;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tracing::{info, warn};

/// Observability HTTP server.
pub struct ObservabilityServer {
    addr: SocketAddr,
    health_checker: Arc<HealthChecker>,
}

impl ObservabilityServer {
    /// Creates an observability server bound to `0.0.0.0:port`.
    #[must_use]
    pub fn new(port: u16) -> Self {
        let addr = SocketAddr::from(([0, 0, 0, 0], port));
        Self {
            addr,
            health_checker: Arc::new(HealthChecker::new(env!("CARGO_PKG_VERSION"))),
        }
    }

    /// Returns the health checker used by the server.
    #[must_use]
    pub fn health_checker(&self) -> Arc<HealthChecker> {
        Arc::clone(&self.health_checker)
    }

    /// Runs the blocking HTTP server until the process exits.
    pub fn run(self) -> std::io::Result<()> {
        let listener = TcpListener::bind(self.addr)?;
        info!("observability server listening on {}", self.addr);
        info!("metrics endpoint: http://{}/metrics", self.addr);
        info!("health endpoint: http://{}/health", self.addr);

        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let checker = Arc::clone(&self.health_checker);
                    let spawn_result = thread::Builder::new()
                        .name("metrics-request".to_string())
                        .stack_size(128 * 1024)
                        .spawn(move || handle_http(stream, checker));
                    if let Err(error) = spawn_result {
                        warn!(%error, "failed to spawn metrics request thread");
                    }
                }
                Err(error) => warn!(%error, "observability accept failed"),
            }
        }
        Ok(())
    }
}

/// Handles one HTTP request and closes the connection.
fn handle_http(mut stream: TcpStream, checker: Arc<HealthChecker>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));

    let mut request = Vec::with_capacity(512);
    let mut buffer = [0u8; 512];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                request.extend_from_slice(&buffer[..n]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") || request.len() > 8192 {
                    break;
                }
            }
            Err(_) => return,
        }
    }

    let request_line = String::from_utf8_lossy(&request);
    let path = request_line
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");

    let (status, content_type, body) = route(path, &checker);
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "OK",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    if stream.write_all(response.as_bytes()).is_ok() {
        let _ = stream.write_all(body.as_bytes());
        let _ = stream.flush();
    }
}

/// Routes one request path to a status, content type, and body.
fn route(path: &str, checker: &HealthChecker) -> (u16, &'static str, String) {
    match path {
        "/metrics" => match METRICS.export() {
            Ok(metrics) => (200, "text/plain; version=0.0.4", metrics),
            Err(error) => (
                500,
                "text/plain",
                format!("failed to export metrics: {error}"),
            ),
        },
        "/health" => {
            let details = HealthDetails {
                active_connections: GLOBAL_METRICS.active_connections(),
                total_orders: counter_sum(&METRICS.orders_total),
                total_trades: counter_sum(&METRICS.trades_total),
            };
            let response = checker.check_health_detailed(details);
            let status = match response.status {
                HealthStatus::Healthy | HealthStatus::Degraded => 200,
                HealthStatus::Unhealthy => 503,
            };
            let body = serde_json::to_string(&response)
                .unwrap_or_else(|error| format!(r#"{{"status":"unhealthy","error":"{error}"}}"#));
            (status, "application/json", body)
        }
        "/health/ready" => {
            if checker.check_readiness() {
                (200, "text/plain", "ready\n".to_string())
            } else {
                (503, "text/plain", "not ready\n".to_string())
            }
        }
        "/health/live" => (200, "text/plain", "live\n".to_string()),
        _ => (404, "text/plain", "not found\n".to_string()),
    }
}

/// Sums the current value of every label combination of `counter`.
fn counter_sum(counter: &prometheus::CounterVec) -> u64 {
    use prometheus::core::Collector;

    counter
        .collect()
        .iter()
        .flat_map(prometheus::proto::MetricFamily::get_metric)
        .map(|metric| metric.get_counter().value() as u64)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_observability_server_creation() {
        let server = ObservabilityServer::new(9090);
        assert_eq!(server.addr.port(), 9090);
    }

    #[test]
    fn test_metric_route() {
        let checker = HealthChecker::new("1.0.0");
        let (status, content_type, body) = route("/metrics", &checker);
        assert_eq!(status, 200);
        assert!(content_type.starts_with("text/plain"));
        assert!(body.contains("matching_engine_orders_total"));
    }

    #[test]
    fn test_health_and_unknown_routes() {
        let checker = HealthChecker::new("1.0.0");
        let (status, content_type, body) = route("/health", &checker);
        assert_eq!(status, 200);
        assert_eq!(content_type, "application/json");
        assert!(body.contains("\"status\":\"healthy\""));

        assert_eq!(route("/unknown", &checker).0, 404);
        assert_eq!(route("/health/live", &checker).0, 200);
    }
}
