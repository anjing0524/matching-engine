//! Minimal gateway TCP listener.
//!
//! Accepts gateway connections and forwards decoded fixed-size frames to
//! middleware. Session registration, authentication, and private responses are
//! layered above this event stream; this slice only owns the accept/read loop.

use crate::exchange::combo_engine::ComboPrivateFill;
use crate::exchange::gateway::{GATEWAY_FRAME_SIZE, GatewayFrame};
use crate::exchange::private_events::PrivateEvent;
use std::collections::HashMap;
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

/// Handle used to address and stop a running listener.
pub struct GatewayServerHandle {
    addr: std::net::SocketAddr,
    shutdown: Arc<AtomicBool>,
    connections: Arc<Mutex<HashMap<u64, TcpStream>>>,
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
        let mut connections = self
            .connections
            .lock()
            .map_err(|_| io::Error::other("gateway connection lock poisoned"))?;
        let stream = connections
            .get_mut(&connection_id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "gateway not connected"))?;
        stream.write_all(&frame.encode())
    }

    /// Sends one private order event to a gateway connection.
    ///
    /// # Errors
    /// Returns an I/O error when the connection is unknown or the write fails.
    pub fn send_private(&self, connection_id: u64, event: &PrivateEvent) -> io::Result<()> {
        self.send(connection_id, GatewayFrame::PrivateEvent(event.clone()))
    }

    /// Closes one gateway connection and removes it from the write map.
    pub fn close(&self, connection_id: u64) {
        if let Ok(mut connections) = self.connections.lock()
            && let Some(stream) = connections.remove(&connection_id)
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
    let connections = Arc::new(Mutex::new(HashMap::new()));
    let accept_connections = Arc::clone(&connections);
    let join = thread::Builder::new()
        .name("gateway-tcp-listener".to_string())
        .spawn(move || {
            while !accept_shutdown.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let id = next_connection.fetch_add(1, Ordering::Relaxed);
                        let event_tx = tx.clone();
                        let write_stream = match stream.try_clone() {
                            Ok(stream) => stream,
                            Err(_) => continue,
                        };
                        if let Ok(mut map) = accept_connections.lock() {
                            map.insert(id, write_stream);
                        }
                        let remove_connections = Arc::clone(&accept_connections);
                        let _ = thread::Builder::new()
                            .name(format!("gateway-tcp-{id}"))
                            .spawn(move || {
                                serve_connection(id, stream, event_tx);
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
            join: Some(join),
        },
        rx,
    ))
}

fn serve_connection(id: u64, mut stream: TcpStream, tx: Sender<GatewayEvent>) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_nodelay(true);
    let mut frame = [0u8; GATEWAY_FRAME_SIZE];
    loop {
        if stream.read_exact(&mut frame).is_err() {
            break;
        }
        match GatewayFrame::decode(&frame) {
            Ok(frame) => {
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
    use crate::exchange::gateway::GatewayFrame;
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
}
