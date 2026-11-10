//! Non-blocking Linux gateway TCP listener.
//!
//! One event-loop thread owns the sockets. The matching core stays synchronous
//! and is not involved in readiness or write backpressure.

use crate::shared::collections::{U64MixMap, U64MixSet};
use crate::trading::combo::engine::ComboPrivateFill;
use crate::trading::core::private_events::PrivateEvent;
use crate::trading::gateway::{GATEWAY_FRAME_SIZE, GatewayFrame};
use mio::net::{TcpListener, TcpStream};
use mio::{Events, Interest, Poll, Token, Waker};
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const LISTENER_TOKEN: Token = Token(0);
const WAKER_TOKEN: Token = Token(1);
const FIRST_CONNECTION_TOKEN: usize = 2;
/// Slow-client protection: disconnect instead of growing an unbounded queue.
const MAX_OUTGOING_BYTES: usize = 1 << 20;

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

enum WriterCommand {
    Send(u64, GatewayFrame),
    Close(u64),
    Stop,
}

struct Connection {
    stream: TcpStream,
    incoming: Vec<u8>,
    outgoing: VecDeque<u8>,
    write_interest: bool,
}

/// Handle used to address and stop a running listener.
pub struct GatewayServerHandle {
    addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
    writer: Sender<WriterCommand>,
    waker: Waker,
    connected: Arc<Mutex<U64MixSet>>,
    join: Option<thread::JoinHandle<()>>,
}

impl GatewayServerHandle {
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Queues one frame for a connected gateway.
    ///
    /// # Errors
    /// Returns an I/O error when the connection is unknown or the event loop
    /// has stopped.
    pub fn send(&self, connection_id: u64, frame: GatewayFrame) -> io::Result<()> {
        if !self
            .connected
            .lock()
            .map_err(|_| io::Error::other("gateway connection lock poisoned"))?
            .contains(&connection_id)
        {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "gateway not connected",
            ));
        }
        self.writer
            .send(WriterCommand::Send(connection_id, frame))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "gateway loop stopped"))?;
        self.waker
            .wake()
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "gateway wake failed"))
    }

    pub fn send_private(&self, connection_id: u64, event: &PrivateEvent) -> io::Result<()> {
        self.send(connection_id, GatewayFrame::PrivateEvent(event.clone()))
    }

    pub fn send_combo_private(
        &self,
        connection_id: u64,
        fill: &ComboPrivateFill,
    ) -> io::Result<()> {
        self.send(connection_id, GatewayFrame::ComboPrivate(*fill))
    }

    pub fn send_redirect(&self, connection_id: u64, address: &str) -> io::Result<()> {
        self.send(
            connection_id,
            GatewayFrame::Redirect {
                address: address.to_string(),
            },
        )
    }

    /// Closes one gateway connection.
    pub fn close(&self, connection_id: u64) {
        if let Ok(mut connected) = self.connected.lock() {
            connected.remove(&connection_id);
        }
        let _ = self.writer.send(WriterCommand::Close(connection_id));
        let _ = self.waker.wake();
    }

    pub fn stop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        let _ = self.writer.send(WriterCommand::Stop);
        let _ = self.waker.wake();
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

struct IoLoop {
    poll: Poll,
    listener: TcpListener,
    connections: U64MixMap<Connection>,
    connected: Arc<Mutex<U64MixSet>>,
    event_tx: Sender<GatewayEvent>,
    next_id: u64,
}

impl IoLoop {
    fn accept_pending(&mut self) {
        loop {
            match self.listener.accept() {
                Ok((mut stream, _)) => {
                    let _ = stream.set_nodelay(true);
                    let id = self.next_id;
                    self.next_id += 1;
                    let token = Token(
                        FIRST_CONNECTION_TOKEN + usize::try_from(id.saturating_sub(1)).unwrap_or(0),
                    );
                    if self
                        .poll
                        .registry()
                        .register(&mut stream, token, Interest::READABLE)
                        .is_err()
                    {
                        continue;
                    }
                    if let Ok(mut connected) = self.connected.lock() {
                        connected.insert(id);
                    }
                    self.connections.insert(
                        id,
                        Connection {
                            stream,
                            incoming: Vec::with_capacity(GATEWAY_FRAME_SIZE * 2),
                            outgoing: VecDeque::new(),
                            write_interest: false,
                        },
                    );
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
    }

    fn handle_connection(&mut self, token: Token) {
        if token.0 < FIRST_CONNECTION_TOKEN {
            return;
        }
        let id = (token.0 - FIRST_CONNECTION_TOKEN) as u64 + 1;
        let Some(mut connection) = self.connections.remove(&id) else {
            return;
        };
        let mut frames = Vec::new();
        let mut closed = false;
        self.read_connection(&mut connection, &mut frames, &mut closed);
        if !closed && !connection.outgoing.is_empty() {
            self.write_connection(id, &mut connection, &mut closed);
        }
        if closed {
            let _ = connection.stream.shutdown(std::net::Shutdown::Both);
            if let Ok(mut connected) = self.connected.lock() {
                connected.remove(&id);
            }
            let _ = self
                .event_tx
                .send(GatewayEvent::Closed { connection_id: id });
            return;
        }
        self.connections.insert(id, connection);
        for frame in frames {
            if self
                .event_tx
                .send(GatewayEvent::Frame {
                    connection_id: id,
                    frame,
                })
                .is_err()
            {
                return;
            }
        }
    }

    fn read_connection(
        &self,
        connection: &mut Connection,
        frames: &mut Vec<GatewayFrame>,
        closed: &mut bool,
    ) {
        let mut buffer = [0u8; GATEWAY_FRAME_SIZE * 4];
        loop {
            match connection.stream.read(&mut buffer) {
                Ok(0) => {
                    *closed = true;
                    break;
                }
                Ok(count) => {
                    connection.incoming.extend_from_slice(&buffer[..count]);
                    if count < buffer.len() {
                        break;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => {
                    *closed = true;
                    return;
                }
            }
        }
        while connection.incoming.len() >= GATEWAY_FRAME_SIZE {
            let mut bytes = [0u8; GATEWAY_FRAME_SIZE];
            bytes.copy_from_slice(&connection.incoming[..GATEWAY_FRAME_SIZE]);
            connection.incoming.drain(..GATEWAY_FRAME_SIZE);
            match GatewayFrame::decode(&bytes) {
                Ok(frame) => frames.push(frame),
                Err(_) => {
                    *closed = true;
                    return;
                }
            }
        }
    }

    fn write_connection(&self, connection_id: u64, connection: &mut Connection, closed: &mut bool) {
        while !connection.outgoing.is_empty() {
            let data = connection.outgoing.make_contiguous();
            match connection.stream.write(data) {
                Ok(0) => {
                    *closed = true;
                    return;
                }
                Ok(count) => {
                    connection.outgoing.drain(..count);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => {
                    *closed = true;
                    return;
                }
            }
        }
        let interest = if connection.outgoing.is_empty() {
            Interest::READABLE
        } else {
            Interest::READABLE | Interest::WRITABLE
        };
        let token = Token(
            FIRST_CONNECTION_TOKEN + usize::try_from(connection_id.saturating_sub(1)).unwrap_or(0),
        );
        let _ = self
            .poll
            .registry()
            .reregister(&mut connection.stream, token, interest);
    }

    fn process_writer_commands(&mut self, receiver: &Receiver<WriterCommand>) -> bool {
        loop {
            match receiver.try_recv() {
                Ok(WriterCommand::Send(connection_id, frame)) => {
                    let mut overflow = false;
                    if let Some(connection) = self.connections.get_mut(&connection_id) {
                        connection.outgoing.extend(frame.encode());
                        overflow = connection.outgoing.len() > MAX_OUTGOING_BYTES;
                        if !overflow && !connection.write_interest {
                            let token = Token(
                                FIRST_CONNECTION_TOKEN
                                    + usize::try_from(connection_id.saturating_sub(1)).unwrap_or(0),
                            );
                            let _ = self.poll.registry().reregister(
                                &mut connection.stream,
                                token,
                                Interest::READABLE | Interest::WRITABLE,
                            );
                            connection.write_interest = true;
                        }
                    }
                    if overflow {
                        self.close_connection(connection_id);
                    }
                }
                Ok(WriterCommand::Close(connection_id)) => {
                    self.close_connection(connection_id);
                }
                Ok(WriterCommand::Stop) => return false,
                Err(std::sync::mpsc::TryRecvError::Empty) => return true,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return false,
            }
        }
    }

    fn close_connection(&mut self, connection_id: u64) {
        let connection = self.connections.remove(&connection_id);
        if let Some(connection) = connection {
            let _ = connection.stream.shutdown(std::net::Shutdown::Both);
        }
        if let Ok(mut connected) = self.connected.lock() {
            connected.remove(&connection_id);
        }
        let _ = self.event_tx.send(GatewayEvent::Closed { connection_id });
    }
}

/// Starts a gateway listener and returns its handle plus an event receiver.
///
/// # Errors
/// Returns an I/O error if the listener or event loop cannot be created.
pub fn start(bind_addr: &str) -> io::Result<(GatewayServerHandle, Receiver<GatewayEvent>)> {
    let listener = std::net::TcpListener::bind(bind_addr)?;
    listener.set_nonblocking(true)?;
    let addr = listener.local_addr()?;
    let poll = Poll::new()?;
    let mut listener = TcpListener::from_std(listener);
    poll.registry()
        .register(&mut listener, LISTENER_TOKEN, Interest::READABLE)?;
    let waker = Waker::new(poll.registry(), WAKER_TOKEN)?;
    let (writer, writer_rx) = channel();
    let (event_tx, event_rx) = channel();
    let connected = Arc::new(Mutex::new(U64MixSet::default()));
    let shutdown = Arc::new(AtomicBool::new(false));
    let thread_shutdown = Arc::clone(&shutdown);
    let thread_connected = Arc::clone(&connected);
    let join = thread::Builder::new()
        .name("gateway-event-loop".to_string())
        .spawn(move || {
            let mut state = IoLoop {
                poll,
                listener,
                connections: U64MixMap::default(),
                connected: thread_connected,
                event_tx,
                next_id: 1,
            };
            let mut events = Events::with_capacity(1024);
            while !thread_shutdown.load(Ordering::Acquire) {
                let _ = state
                    .poll
                    .poll(&mut events, Some(Duration::from_millis(100)));
                for event in events.iter() {
                    match event.token() {
                        LISTENER_TOKEN => state.accept_pending(),
                        WAKER_TOKEN => {}
                        token => state.handle_connection(token),
                    }
                }
                if !state.process_writer_commands(&writer_rx) {
                    break;
                }
            }
        })?;
    Ok((
        GatewayServerHandle {
            addr,
            shutdown,
            writer,
            waker,
            connected,
            join: Some(join),
        },
        event_rx,
    ))
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
        let mut stream = std::net::TcpStream::connect(handle.addr()).unwrap();
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
        let mut stream = std::net::TcpStream::connect(handle.addr()).unwrap();
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
        let connection_id = match event {
            GatewayEvent::Frame { connection_id, .. } => connection_id,
            other => panic!("expected frame, got {other:?}"),
        };
        let handle = Arc::new(handle);

        let mut writers = Vec::new();
        for thread_id in 0..4u64 {
            let handle = Arc::clone(&handle);
            writers.push(std::thread::spawn(move || {
                for index in 0..100u64 {
                    let seq = thread_id * 100 + index;
                    handle
                        .send(connection_id, GatewayFrame::Heartbeat { seq })
                        .unwrap();
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
    #[test]
    #[ignore = "multi-connection gateway profile; run explicitly"]
    fn many_connections_profile() {
        use std::collections::HashSet;
        use std::time::Instant;

        let count: usize = std::env::var("GATEWAY_CONNECTIONS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(4096);
        let (handle, rx) = start("127.0.0.1:0").unwrap();
        let mut clients = Vec::with_capacity(count);
        for _ in 0..count {
            let mut stream = std::net::TcpStream::connect(handle.addr()).unwrap();
            stream
                .write_all(
                    &GatewayFrame::Login {
                        member_id: 1,
                        auth_token: 0,
                        user_id: 1,
                        seat_id: 1,
                        session_epoch: 1,
                        last_private_seq: 0,
                    }
                    .encode(),
                )
                .unwrap();
            clients.push(stream);
        }
        let start = Instant::now();
        let mut ids = HashSet::new();
        for _ in 0..count {
            match rx.recv_timeout(Duration::from_secs(10)).unwrap() {
                GatewayEvent::Frame { connection_id, .. } => {
                    ids.insert(connection_id);
                }
                GatewayEvent::Closed { .. } => panic!("connection closed early"),
            }
        }
        println!(
            "gateway_many: {count} connections in {:?}, unique_ids={}",
            start.elapsed(),
            ids.len()
        );
        assert_eq!(ids.len(), count);
    }
    #[test]
    fn login_then_eof_emits_frame_then_closed() {
        let (handle, rx) = start("127.0.0.1:0").unwrap();
        let mut stream = std::net::TcpStream::connect(handle.addr()).unwrap();
        stream
            .write_all(
                &GatewayFrame::Login {
                    member_id: 1,
                    auth_token: 0,
                    user_id: 1,
                    seat_id: 1,
                    session_epoch: 1,
                    last_private_seq: 0,
                }
                .encode(),
            )
            .unwrap();
        drop(stream);
        let first = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(first, GatewayEvent::Frame { .. }));
        let second = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(second, GatewayEvent::Closed { .. }));
    }
}
