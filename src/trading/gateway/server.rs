//! Minimal gateway TCP listener.
//!
//! Accepts gateway connections and forwards decoded fixed-size frames to
//! middleware. Session registration, authentication, and private responses are
//! layered above this event stream; this slice only owns the accept/read loop.

use crate::shared::collections::U64MixMap;
use crate::trading::combo::engine::ComboPrivateFill;
use crate::trading::core::private_events::PrivateEvent;
use crate::trading::gateway::{GATEWAY_FRAME_SIZE, GatewayFrame};
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Event emitted by the gateway listener.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GatewayEvent {
    Frame {
        connection_id: u64,
        frame: GatewayFrame,
    },
    Closed {
        connection_id: u64,
    },
}

type SharedStream = Arc<Mutex<TcpStream>>;

/// Cumulative gateway socket counters.
#[derive(Debug, Default)]
pub struct GatewayStats {
    accepted: AtomicU64,
    frames_read: AtomicU64,
    writes: AtomicU64,
    bytes_written: AtomicU64,
    write_errors: AtomicU64,
    read_ns: AtomicU64,
    read_calls: AtomicU64,
    write_ns: AtomicU64,
    timing: AtomicBool,
}

/// Snapshot of [`GatewayStats`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GatewayStatsSnapshot {
    pub accepted: u64,
    pub frames_read: u64,
    pub writes: u64,
    pub bytes_written: u64,
    pub write_errors: u64,
    pub read_ns: u64,
    pub read_calls: u64,
    pub write_ns: u64,
}

impl GatewayStats {
    fn snapshot(&self) -> GatewayStatsSnapshot {
        GatewayStatsSnapshot {
            accepted: self.accepted.load(Ordering::Relaxed),
            frames_read: self.frames_read.load(Ordering::Relaxed),
            writes: self.writes.load(Ordering::Relaxed),
            bytes_written: self.bytes_written.load(Ordering::Relaxed),
            write_errors: self.write_errors.load(Ordering::Relaxed),
            read_ns: self.read_ns.load(Ordering::Relaxed),
            read_calls: self.read_calls.load(Ordering::Relaxed),
            write_ns: self.write_ns.load(Ordering::Relaxed),
        }
    }

    fn timing_enabled(&self) -> bool {
        self.timing.load(Ordering::Relaxed)
    }
}

/// Handle used to address and stop a running listener.
pub struct GatewayServerHandle {
    addr: std::net::SocketAddr,
    shutdown: Arc<AtomicBool>,
    connections: Arc<Mutex<U64MixMap<SharedStream>>>,
    stats: Arc<GatewayStats>,
    join: Option<thread::JoinHandle<()>>,
}

impl GatewayServerHandle {
    #[must_use]
    pub fn addr(&self) -> std::net::SocketAddr {
        self.addr
    }

    /// Sends one frame to a connected gateway.
    ///
    /// # Errors
    /// Returns an I/O error when the connection is unknown or the write fails.
    pub fn send(&self, connection_id: u64, frame: GatewayFrame) -> io::Result<()> {
        let stream = {
            let connections = self
                .connections
                .lock()
                .map_err(|_| io::Error::other("gateway connection lock poisoned"))?;
            connections
                .get(&connection_id)
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "gateway not connected"))?
        };
        let encoded = frame.encode();
        let timing = self.stats.timing_enabled();
        let start = timing.then(std::time::Instant::now);
        let mut stream = stream
            .lock()
            .map_err(|_| io::Error::other("gateway stream lock poisoned"))?;
        let result = stream.write_all(&encoded);
        if let Some(start) = start {
            self.stats
                .write_ns
                .fetch_add(start.elapsed().as_nanos() as u64, Ordering::Relaxed);
        }
        match result {
            Ok(()) => {
                self.stats.writes.fetch_add(1, Ordering::Relaxed);
                self.stats
                    .bytes_written
                    .fetch_add(encoded.len() as u64, Ordering::Relaxed);
                Ok(())
            }
            Err(error) => {
                self.stats.write_errors.fetch_add(1, Ordering::Relaxed);
                Err(error)
            }
        }
    }

    /// Enables read/write timing counters (diagnostic mode).
    pub fn enable_timing(&self) {
        self.stats.timing.store(true, Ordering::Relaxed);
    }

    #[must_use]
    pub fn stats(&self) -> GatewayStatsSnapshot {
        self.stats.snapshot()
    }

    /// Sends one private order event to a gateway connection.
    ///
    /// # Errors
    /// Returns an I/O error when the connection is unknown or the write fails.
    /// Sends a batch of frames with one socket write.
    ///
    /// # Errors
    /// Returns an I/O error when the connection is unknown or the write fails.
    pub fn send_batch(&self, connection_id: u64, frames: &[GatewayFrame]) -> io::Result<()> {
        if frames.is_empty() {
            return Ok(());
        }
        let stream = {
            let connections = self
                .connections
                .lock()
                .map_err(|_| io::Error::other("gateway connection lock poisoned"))?;
            connections
                .get(&connection_id)
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "gateway not connected"))?
        };
        let mut encoded = Vec::with_capacity(frames.len() * GATEWAY_FRAME_SIZE);
        for frame in frames {
            encoded.extend_from_slice(&frame.clone().encode());
        }
        let timing = self.stats.timing_enabled();
        let start = timing.then(std::time::Instant::now);
        let mut stream = stream
            .lock()
            .map_err(|_| io::Error::other("gateway stream lock poisoned"))?;
        let result = stream.write_all(&encoded);
        if let Some(start) = start {
            self.stats
                .write_ns
                .fetch_add(start.elapsed().as_nanos() as u64, Ordering::Relaxed);
        }
        match result {
            Ok(()) => {
                self.stats
                    .writes
                    .fetch_add(frames.len() as u64, Ordering::Relaxed);
                self.stats
                    .bytes_written
                    .fetch_add(encoded.len() as u64, Ordering::Relaxed);
                Ok(())
            }
            Err(error) => {
                self.stats
                    .write_errors
                    .fetch_add(frames.len() as u64, Ordering::Relaxed);
                Err(error)
            }
        }
    }

    pub fn send_private(&self, connection_id: u64, event: &PrivateEvent) -> io::Result<()> {
        self.send(connection_id, GatewayFrame::PrivateEvent(event.clone()))
    }

    /// Closes one gateway connection and removes it from the write map.
    pub fn close(&self, connection_id: u64) {
        let stream = self
            .connections
            .lock()
            .ok()
            .and_then(|mut connections| connections.remove(&connection_id));
        if let Some(stream) = stream
            && let Ok(stream) = stream.lock()
        {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }

    /// Sends one combination private fill to a gateway connection.
    ///
    /// # Errors
    /// Returns an I/O error when the connection is unknown or the write fails.
    pub fn send_combo_private(
        &self,
        connection_id: u64,
        fill: &ComboPrivateFill,
    ) -> io::Result<()> {
        self.send(connection_id, GatewayFrame::ComboPrivate(*fill))
    }

    /// Sends a redirect frame to a gateway connection.
    ///
    /// # Errors
    /// Returns an I/O error when the connection is unknown or the write fails.
    pub fn send_redirect(&self, connection_id: u64, address: &str) -> io::Result<()> {
        self.send(
            connection_id,
            GatewayFrame::Redirect {
                address: address.to_string(),
            },
        )
    }

    pub fn stop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for GatewayServerHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Starts a gateway listener and returns its handle plus the event receiver.
///
/// # Errors
/// Returns an I/O error if the listener cannot bind.
pub fn start(
    bind_addr: &str,
) -> io::Result<(GatewayServerHandle, std::sync::mpsc::Receiver<GatewayEvent>)> {
    let listener = TcpListener::bind(bind_addr)?;
    listener.set_nonblocking(true)?;
    let addr = listener.local_addr()?;
    let shutdown = Arc::new(AtomicBool::new(false));
    let accept_shutdown = Arc::clone(&shutdown);
    let (tx, rx) = channel();
    let next_connection = Arc::new(AtomicU64::new(1));
    let connections = Arc::new(Mutex::new(U64MixMap::default()));
    let stats = Arc::new(GatewayStats::default());
    let accept_connections = Arc::clone(&connections);
    let accept_stats = Arc::clone(&stats);
    let join = thread::Builder::new()
        .name("gateway-tcp-listener".to_string())
        .spawn(move || {
            while !accept_shutdown.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let id = next_connection.fetch_add(1, Ordering::Relaxed);
                        accept_stats.accepted.fetch_add(1, Ordering::Relaxed);
                        let event_tx = tx.clone();
                        let write_stream = match stream.try_clone() {
                            Ok(stream) => Arc::new(Mutex::new(stream)),
                            Err(_) => continue,
                        };
                        if let Ok(mut map) = accept_connections.lock() {
                            map.insert(id, write_stream);
                        }
                        let remove_connections = Arc::clone(&accept_connections);
                        let stats = Arc::clone(&accept_stats);
                        let _ = thread::Builder::new()
                            .name(format!("gateway-tcp-{id}"))
                            .spawn(move || {
                                serve_connection(id, stream, event_tx, stats);
                                if let Ok(mut map) = remove_connections.lock() {
                                    map.remove(&id);
                                }
                            });
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        })?;
    Ok((
        GatewayServerHandle {
            addr,
            shutdown,
            connections,
            stats,
            join: Some(join),
        },
        rx,
    ))
}

fn serve_connection(
    id: u64,
    mut stream: TcpStream,
    tx: Sender<GatewayEvent>,
    stats: Arc<GatewayStats>,
) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_nodelay(true);
    let mut frame = [0u8; GATEWAY_FRAME_SIZE];
    loop {
        let start = stats.timing_enabled().then(std::time::Instant::now);
        if stream.read_exact(&mut frame).is_err() {
            break;
        }
        if let Some(start) = start {
            stats
                .read_ns
                .fetch_add(start.elapsed().as_nanos() as u64, Ordering::Relaxed);
            stats.read_calls.fetch_add(1, Ordering::Relaxed);
        }
        match GatewayFrame::decode(&frame) {
            Ok(frame) => {
                stats.frames_read.fetch_add(1, Ordering::Relaxed);
                if tx
                    .send(GatewayEvent::Frame {
                        connection_id: id,
                        frame,
                    })
                    .is_err()
                {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let _ = tx.send(GatewayEvent::Closed { connection_id: id });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trading::gateway::GatewayFrame;
    use std::io::Write;
    use std::time::Duration;

    #[test]
    fn accepts_login_and_command_frames() {
        let (handle, rx) = start("127.0.0.1:0").unwrap();
        let mut stream = TcpStream::connect(handle.addr()).unwrap();
        stream.set_nodelay(true).unwrap();

        let login = GatewayFrame::Login {
            member_id: 1,
            auth_token: 0,
            user_id: 2,
            seat_id: 3,
            session_epoch: 4,
            last_private_seq: 5,
        };
        stream.write_all(&login.clone().encode()).unwrap();
        let event = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(
            event,
            GatewayEvent::Frame {
                connection_id: 1,
                frame: login
            }
        );

        stream
            .write_all(&GatewayFrame::Heartbeat { seq: 9 }.encode())
            .unwrap();
        let event = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(
            event,
            GatewayEvent::Frame {
                connection_id: 1,
                frame: GatewayFrame::Heartbeat { seq: 9 }
            }
        );
    }
    #[test]
    fn concurrent_writes_keep_frames_intact() {
        let (handle, rx) = start("127.0.0.1:0").unwrap();
        let mut stream = TcpStream::connect(handle.addr()).unwrap();
        stream.set_nodelay(true).unwrap();
        stream
            .write_all(
                &GatewayFrame::Login {
                    member_id: 1,
                    auth_token: 0,
                    user_id: 2,
                    seat_id: 3,
                    session_epoch: 4,
                    last_private_seq: 0,
                }
                .encode(),
            )
            .unwrap();
        let event = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(event, GatewayEvent::Frame { .. }));
        let handle = Arc::new(handle);

        let mut writers = Vec::new();
        for thread_id in 0..4u64 {
            let handle = Arc::clone(&handle);
            writers.push(std::thread::spawn(move || {
                for index in 0..100u64 {
                    let seq = thread_id * 100 + index;
                    handle.send(1, GatewayFrame::Heartbeat { seq }).unwrap();
                }
            }));
        }
        for _ in 0..400 {
            let mut bytes = [0u8; GATEWAY_FRAME_SIZE];
            stream.read_exact(&mut bytes).unwrap();
            assert!(matches!(
                GatewayFrame::decode(&bytes).unwrap(),
                GatewayFrame::Heartbeat { .. }
            ));
        }
        for writer in writers {
            writer.join().unwrap();
        }
    }
}
