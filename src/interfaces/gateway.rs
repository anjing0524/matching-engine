//! Blocking TCP gateway.
//!
//! The gateway owns the listener, accept loop, connection registry, connection
//! limit, shutdown/fatal control, and the lifecycle of session threads. It knows
//! nothing about orders or matching: callers provide one session closure.
use crate::infrastructure::network::{TcpConnection, TcpTransport};
use crate::infrastructure::observability::GLOBAL_METRICS;
use std::collections::HashMap;
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tracing::{error, info, warn};

/// Gateway configuration.
#[derive(Debug, Clone, Copy)]
pub struct GatewayConfig {
    /// Listen address.
    pub addr: SocketAddr,
    /// Maximum concurrently connected sessions.
    pub max_connections: usize,
    /// Stack size for each session thread.
    pub session_stack_size: usize,
}

/// Errors that terminate the gateway accept loop.
#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Result of running the gateway until shutdown.
#[derive(Debug, Default)]
pub struct GatewayOutcome {
    /// Fatal message set by a session or the accept loop.
    pub fatal: Option<String>,
}

/// Shared shutdown/fatal state.
#[derive(Clone)]
pub struct GatewayControl {
    inner: Arc<ControlState>,
}

struct ControlState {
    shutdown: AtomicBool,
    fatal: Mutex<Option<String>>,
}

impl GatewayControl {
    fn new() -> Self {
        Self {
            inner: Arc::new(ControlState {
                shutdown: AtomicBool::new(false),
                fatal: Mutex::new(None),
            }),
        }
    }

    /// Requests a graceful shutdown.
    pub fn request_shutdown(&self) {
        self.inner.shutdown.store(true, Ordering::Release);
    }

    /// Returns whether shutdown has been requested.
    #[must_use]
    pub fn is_shutdown(&self) -> bool {
        self.inner.shutdown.load(Ordering::Acquire)
    }

    /// Records the first fatal error and requests shutdown.
    pub fn set_fatal(&self, message: String) {
        let mut fatal = lock(&self.inner.fatal);
        if fatal.is_none() {
            *fatal = Some(message);
        }
        drop(fatal);
        self.request_shutdown();
    }

    /// Returns the fatal message, if any.
    #[must_use]
    pub fn fatal(&self) -> Option<String> {
        lock(&self.inner.fatal).clone()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Default)]
struct ConnectionRegistry {
    next_id: AtomicU64,
    streams: Mutex<HashMap<u64, TcpStream>>,
}

impl ConnectionRegistry {
    fn register(&self, stream: TcpStream) -> std::io::Result<u64> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        lock(&self.streams).insert(id, stream);
        Ok(id)
    }

    fn remove(&self, id: u64) {
        lock(&self.streams).remove(&id);
    }

    fn shutdown_all(&self) {
        let mut streams = lock(&self.streams);
        for stream in streams.values() {
            let _ = stream.shutdown(Shutdown::Both);
        }
        streams.clear();
    }
}

struct ConnectionGuard {
    active: Arc<AtomicUsize>,
}

impl ConnectionGuard {
    fn new(active: Arc<AtomicUsize>) -> Self {
        GLOBAL_METRICS.inc_connections();
        Self { active }
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        GLOBAL_METRICS.dec_connections();
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

struct RegistryGuard {
    registry: Arc<ConnectionRegistry>,
    id: u64,
}

impl Drop for RegistryGuard {
    fn drop(&mut self) {
        self.registry.remove(self.id);
    }
}

fn try_acquire_connection(active: &AtomicUsize, max: usize) -> bool {
    let mut current = active.load(Ordering::Acquire);
    loop {
        if current >= max {
            return false;
        }
        match active.compare_exchange_weak(
            current,
            current + 1,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return true,
            Err(observed) => current = observed,
        }
    }
}

/// A bound TCP gateway.
pub struct Gateway {
    transport: TcpTransport,
    control: GatewayControl,
    config: GatewayConfig,
    local_addr: SocketAddr,
}

impl Gateway {
    /// Binds the listener.
    ///
    /// # Errors
    /// Returns an error if the address cannot be bound.
    pub fn bind(config: GatewayConfig) -> Result<Self, GatewayError> {
        let mut transport = TcpTransport::new()?;
        transport.bind(config.addr)?;
        let local_addr = transport.local_addr()?;
        Ok(Self {
            transport,
            control: GatewayControl::new(),
            config,
            local_addr,
        })
    }

    /// Returns the bound local address.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Returns a control handle usable from signal/session threads.
    #[must_use]
    pub fn control(&self) -> GatewayControl {
        self.control.clone()
    }

    /// Runs the accept loop until shutdown, then drains sessions.
    ///
    /// `session` is called exactly once per accepted connection, on its own
    /// blocking thread. The closure can clone captured handles as needed.
    ///
    /// # Errors
    /// Returns an error if the listener cannot accept a connection.
    pub fn run<F>(mut self, session: F) -> Result<GatewayOutcome, GatewayError>
    where
        F: Fn(TcpConnection) + Send + Sync + 'static,
    {
        let session = Arc::new(session);
        let registry = Arc::new(ConnectionRegistry::default());
        let active = Arc::new(AtomicUsize::new(0));
        let mut handles: Vec<JoinHandle<()>> = Vec::new();

        info!(addr = %self.local_addr, "gateway listening");

        loop {
            if self.control.is_shutdown() || self.control.fatal().is_some() {
                break;
            }

            match self.transport.accept() {
                Ok(conn) => {
                    if !try_acquire_connection(&active, self.config.max_connections) {
                        warn!("connection limit reached; dropping connection");
                        continue;
                    }

                    let peer = conn.peer_addr();
                    let id = match conn
                        .try_clone()
                        .and_then(|stream| registry.register(stream))
                    {
                        Ok(id) => id,
                        Err(error) => {
                            error!(%error, "failed to register connection");
                            active.fetch_sub(1, Ordering::AcqRel);
                            continue;
                        }
                    };

                    let active_guard = ConnectionGuard::new(Arc::clone(&active));
                    let registry_guard = RegistryGuard {
                        registry: Arc::clone(&registry),
                        id,
                    };
                    let session = Arc::clone(&session);
                    let spawn_result = thread::Builder::new()
                        .name(format!("client-{peer}"))
                        .stack_size(self.config.session_stack_size)
                        .spawn(move || {
                            let _active_guard = active_guard;
                            let _registry_guard = registry_guard;
                            session(conn);
                        });

                    match spawn_result {
                        Ok(handle) => handles.push(handle),
                        Err(error) => {
                            error!(%error, "failed to spawn session thread");
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => {
                    self.control
                        .set_fatal(format!("gateway accept failed: {error}"));
                    break;
                }
            }
        }

        self.control.request_shutdown();
        registry.shutdown_all();
        for handle in handles {
            let _ = handle.join();
        }

        Ok(GatewayOutcome {
            fatal: self.control.fatal(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream};

    #[test]
    fn test_gateway_accepts_and_shuts_down() {
        let gateway = Gateway::bind(GatewayConfig {
            addr: "127.0.0.1:0".parse().unwrap(),
            max_connections: 2,
            session_stack_size: 64 * 1024,
        })
        .unwrap();
        let addr: SocketAddr = gateway.local_addr();
        let control = gateway.control();
        let server = thread::spawn(move || {
            gateway
                .run(move |mut conn| {
                    let _ = conn.send(b"ok");
                })
                .unwrap()
        });

        thread::sleep(Duration::from_millis(10));
        let mut client = TcpStream::connect(addr).unwrap();
        client.write_all(&2u32.to_be_bytes()).unwrap();
        let mut len = [0u8; 4];
        let mut buf = [0u8; 2];
        // The session only sends; read its frame directly.
        let _ = client.read_exact(&mut len);
        let _ = client.read(&mut buf);
        control.request_shutdown();
        let outcome = server.join().unwrap();
        assert!(outcome.fatal.is_none());
    }
}
