//! Gateway endpoint directory and TCP health probes for client redirection.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// Round-robin directory of gateway TCP endpoints.
#[derive(Debug)]
pub struct GatewayDirectory {
    endpoints: Vec<SocketAddr>,
    next: AtomicUsize,
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

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
