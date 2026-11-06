//! Gateway endpoint directory and TCP health probes for client redirection.

use std::collections::HashMap;
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use std::time::Instant;

/// Round-robin directory of gateway TCP endpoints.
#[derive(Debug)]
pub struct GatewayDirectory {
    endpoints: Vec<SocketAddr>,
    next: AtomicUsize,
    health: Mutex<HashMap<SocketAddr, (bool, Instant)>>,
}

impl GatewayDirectory {
    /// # Errors
    /// Returns a parse error for any malformed endpoint.
    pub fn new(
        endpoints: impl IntoIterator<Item = String>,
    ) -> Result<Self, std::net::AddrParseError> {
        let endpoints = endpoints
            .into_iter()
            .map(|endpoint| endpoint.parse())
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            endpoints,
            next: AtomicUsize::new(0),
            health: Mutex::new(HashMap::new()),
        })
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.endpoints.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.endpoints.len()
    }

    /// Returns the next endpoint, round-robin.
    #[must_use]
    pub fn next(&self) -> Option<SocketAddr> {
        if self.endpoints.is_empty() {
            return None;
        }
        let index = self.next.fetch_add(1, Ordering::Relaxed) % self.endpoints.len();
        Some(self.endpoints[index])
    }

    /// TCP-probes every endpoint with a bounded timeout.
    #[must_use]
    pub fn probe_all(&self, timeout: Duration) -> Vec<(SocketAddr, bool)> {
        self.endpoints
            .iter()
            .map(|endpoint| (*endpoint, probe(*endpoint, timeout)))
            .collect()
    }

    /// Uses a cached health result newer than `ttl`, otherwise probes.
    #[must_use]
    pub fn is_healthy_cached(
        &self,
        endpoint: SocketAddr,
        timeout: Duration,
        ttl: Duration,
    ) -> bool {
        let now = Instant::now();
        if let Ok(cache) = self.health.lock()
            && let Some((healthy, checked_at)) = cache.get(&endpoint)
            && now.duration_since(*checked_at) <= ttl
        {
            return *healthy;
        }
        let healthy = probe(endpoint, timeout);
        if let Ok(mut cache) = self.health.lock() {
            cache.insert(endpoint, (healthy, now));
        }
        healthy
    }

    /// Re-probes every endpoint and refreshes the cache.
    #[must_use]
    pub fn refresh_health(&self, timeout: Duration) -> Vec<(SocketAddr, bool)> {
        let now = Instant::now();
        let results: Vec<_> = self
            .endpoints
            .iter()
            .map(|endpoint| (*endpoint, probe(*endpoint, timeout)))
            .collect();
        if let Ok(mut cache) = self.health.lock() {
            for (endpoint, healthy) in &results {
                cache.insert(*endpoint, (*healthy, now));
            }
        }
        results
    }

    /// Drops cached health state.
    pub fn clear_health_cache(&self) {
        if let Ok(mut cache) = self.health.lock() {
            cache.clear();
        }
    }

    /// Returns the first endpoint with a fresh/refreshed healthy state.
    #[must_use]
    pub fn first_healthy_cached(&self, timeout: Duration, ttl: Duration) -> Option<SocketAddr> {
        self.endpoints
            .iter()
            .find(|endpoint| self.is_healthy_cached(**endpoint, timeout, ttl))
            .copied()
    }

    /// Returns the first endpoint that accepts a TCP connection.
    #[must_use]
    pub fn first_healthy(&self, timeout: Duration) -> Option<SocketAddr> {
        self.endpoints
            .iter()
            .find(|endpoint| probe(**endpoint, timeout))
            .copied()
    }
}

fn probe(endpoint: SocketAddr, timeout: Duration) -> bool {
    TcpStream::connect_timeout(&endpoint, timeout).is_ok()
}

/// Background health refresher for a shared directory.
pub struct HealthMonitor {
    shutdown: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl HealthMonitor {
    pub fn start(directory: Arc<GatewayDirectory>, interval: Duration, timeout: Duration) -> Self {
        let shutdown = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&shutdown);
        let join = std::thread::Builder::new()
            .name("gateway-health".to_string())
            .spawn(move || {
                while !flag.load(Ordering::Acquire) {
                    let _ = directory.refresh_health(timeout);
                    std::thread::sleep(interval);
                }
            })
            .ok();
        Self { shutdown, join }
    }

    pub fn stop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn round_robins_and_probes() {
        let live = TcpListener::bind("127.0.0.1:0").unwrap();
        let live_addr = live.local_addr().unwrap();
        let dead = TcpListener::bind("127.0.0.1:0").unwrap();
        let dead_addr = dead.local_addr().unwrap();
        drop(dead);

        let directory =
            GatewayDirectory::new([dead_addr.to_string(), live_addr.to_string()]).unwrap();
        assert_eq!(directory.len(), 2);
        assert_eq!(directory.next(), Some(dead_addr));
        assert_eq!(directory.next(), Some(live_addr));
        assert_eq!(directory.next(), Some(dead_addr));

        let health = directory.probe_all(Duration::from_millis(200));
        assert!(!health[0].1);
        assert!(health[1].1);
        assert_eq!(
            directory.first_healthy(Duration::from_millis(200)),
            Some(live_addr)
        );
        assert!(directory.is_healthy_cached(
            live_addr,
            Duration::from_millis(200),
            Duration::from_secs(5)
        ));
        drop(live);
        // TTL keeps the cached healthy result even though the listener is gone.
        assert!(directory.is_healthy_cached(
            live_addr,
            Duration::from_millis(200),
            Duration::from_secs(5)
        ));
        directory.clear_health_cache();
        assert!(!directory.is_healthy_cached(
            live_addr,
            Duration::from_millis(50),
            Duration::from_secs(5)
        ));
    }
    #[test]
    fn health_monitor_refreshes_cache() {
        let live = TcpListener::bind("127.0.0.1:0").unwrap();
        let live_addr = live.local_addr().unwrap();
        let directory = Arc::new(GatewayDirectory::new([live_addr.to_string()]).unwrap());
        let mut monitor = HealthMonitor::start(
            Arc::clone(&directory),
            Duration::from_millis(10),
            Duration::from_millis(100),
        );
        std::thread::sleep(Duration::from_millis(40));
        assert!(directory.is_healthy_cached(
            live_addr,
            Duration::from_millis(50),
            Duration::from_secs(1)
        ));
        monitor.stop();
    }
}
