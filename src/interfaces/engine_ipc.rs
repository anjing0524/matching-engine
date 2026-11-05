//! Internal gateway→matching-engine IPC.
//!
//! The engine accepts length-framed client requests over a Unix socket and
//! returns one or more already-encoded `ServerResponse` payloads. Gateway and
//! engine are separate processes: this module only moves opaque protocol
//! payloads and keeps request correlation at the gateway boundary.
use crate::application::services::matching_runtime::{
    CancelTicket, MatchingHandle, OrderOutcome, OrderTicket, RuntimeError,
};
use crate::application::services::persistent_service::ServiceError;
use crate::shared::metrics::METRICS;
use crate::shared::protocol::{
    CancelOrderRequest, ClientMessage, ClientRequest, ErrorCode, OrderConfirmation,
    PROTOCOL_VERSION, ServerMessage, ServerResponse,
};
use crate::shared::wire::{decode_frame_payload, encode_to_vec};
use std::io::{self, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// Maximum internal/client payload accepted from a peer.
pub const MAX_IPC_PAYLOAD: usize = 1024 * 1024;

/// One internal request: client connection id plus opaque client frame payload.
#[derive(Debug, Clone)]
pub struct IpcRequest {
    pub client_id: u64,
    pub payload: Vec<u8>,
}

/// One internal response: one or more opaque `ServerResponse` payloads.
#[derive(Debug, Clone)]
pub struct IpcResponse {
    pub client_id: u64,
    pub payloads: Vec<Vec<u8>>,
}

pub fn encode_request(request: &IpcRequest) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + request.payload.len());
    out.extend_from_slice(&((8 + request.payload.len()) as u32).to_be_bytes());
    out.extend_from_slice(&request.client_id.to_be_bytes());
    out.extend_from_slice(&request.payload);
    out
}

pub fn encode_response(response: &IpcResponse) -> Vec<u8> {
    let body_len = 8
        + 2
        + response
            .payloads
            .iter()
            .map(|payload| 4 + payload.len())
            .sum::<usize>();
    let mut out = Vec::with_capacity(4 + body_len);
    out.extend_from_slice(&(body_len as u32).to_be_bytes());
    out.extend_from_slice(&response.client_id.to_be_bytes());
    out.extend_from_slice(&(response.payloads.len() as u16).to_be_bytes());
    for payload in &response.payloads {
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(payload);
    }
    out
}

fn take_u32(buf: &[u8], at: usize) -> Option<u32> {
    buf.get(at..at + 4)
        .map(|bytes| u32::from_be_bytes(bytes.try_into().expect("4 bytes")))
}

fn take_payload(buf: &[u8], at: usize) -> Option<(Vec<u8>, usize)> {
    let len = take_u32(buf, at)? as usize;
    if len > MAX_IPC_PAYLOAD {
        return None;
    }
    let start = at + 4;
    let end = start.checked_add(len)?;
    Some((buf.get(start..end)?.to_vec(), end))
}

/// Parses one internal response from a byte buffer.
pub fn decode_response(buf: &[u8]) -> Option<(IpcResponse, usize)> {
    let body_len = take_u32(buf, 0)? as usize;
    if body_len > MAX_IPC_PAYLOAD {
        return None;
    }
    let total = 4 + body_len;
    if buf.len() < total {
        return None;
    }
    let mut at = 4;
    let client_id = u64::from_be_bytes(buf.get(at..at + 8)?.try_into().ok()?);
    at += 8;
    let count = u16::from_be_bytes(buf.get(at..at + 2)?.try_into().ok()?) as usize;
    at += 2;
    let mut payloads = Vec::with_capacity(count);
    for _ in 0..count {
        let (payload, next) = take_payload(buf, at)?;
        payloads.push(payload);
        at = next;
    }
    Some((
        IpcResponse {
            client_id,
            payloads,
        },
        total,
    ))
}

/// Parses one internal request from a byte buffer.
pub fn decode_request(buf: &[u8]) -> Option<(IpcRequest, usize)> {
    let body_len = take_u32(buf, 0)? as usize;
    if !(8..=MAX_IPC_PAYLOAD).contains(&body_len) {
        return None;
    }
    let total = 4 + body_len;
    if buf.len() < total {
        return None;
    }
    let client_id = u64::from_be_bytes(buf.get(4..12)?.try_into().ok()?);
    Some((
        IpcRequest {
            client_id,
            payload: buf[12..total].to_vec(),
        },
        total,
    ))
}

pub fn write_request<W: Write>(writer: &mut W, request: &IpcRequest) -> io::Result<()> {
    writer.write_all(&encode_request(request))?;
    writer.flush()
}

pub fn read_request<R: Read>(reader: &mut R) -> io::Result<Option<IpcRequest>> {
    let Some(body_len) = read_u32(reader)? else {
        return Ok(None);
    };
    let body_len = body_len as usize;
    if !(8..=MAX_IPC_PAYLOAD).contains(&body_len) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid internal request length",
        ));
    }
    let mut body = vec![0u8; body_len];
    reader.read_exact(&mut body)?;
    let client_id = u64::from_be_bytes(body[..8].try_into().expect("8 bytes"));
    Ok(Some(IpcRequest {
        client_id,
        payload: body[8..].to_vec(),
    }))
}

pub fn write_response<W: Write>(writer: &mut W, response: &IpcResponse) -> io::Result<()> {
    writer.write_all(&encode_response(response))?;
    writer.flush()
}

fn read_u32<R: Read>(reader: &mut R) -> io::Result<Option<u32>> {
    let mut buf = [0u8; 4];
    let mut read = 0;
    while read < 4 {
        match reader.read(&mut buf[read..]) {
            Ok(0) if read == 0 => return Ok(None),
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated IPC length",
                ));
            }
            Ok(n) => read += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(Some(u32::from_be_bytes(buf)))
}

/// Configuration for the engine-side Unix socket listener.
#[derive(Debug, Clone)]
pub struct EngineIpcConfig {
    pub path: PathBuf,
    pub max_connections: usize,
    pub symbol: String,
}

/// Starts the engine IPC listener in a background thread.
///
/// # Errors
/// Returns an error if the Unix socket cannot be bound.
pub fn start(
    config: EngineIpcConfig,
    handle: MatchingHandle,
    shutdown: Arc<AtomicBool>,
) -> io::Result<thread::JoinHandle<()>> {
    if let Some(parent) = config.path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(&config.path);
    let listener = UnixListener::bind(&config.path)?;
    listener.set_nonblocking(true)?;
    thread::Builder::new()
        .name("engine-ipc".to_string())
        .spawn(move || {
            let active = Arc::new(AtomicUsize::new(0));
            while !shutdown.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        tracing::info!("engine IPC accepted connection");
                        if active.fetch_add(1, Ordering::AcqRel) >= config.max_connections {
                            active.fetch_sub(1, Ordering::AcqRel);
                            let _ = stream.shutdown(std::net::Shutdown::Both);
                            continue;
                        }
                        let handle = handle.clone();
                        let active = Arc::clone(&active);
                        let symbol = config.symbol.clone();
                        let _ = thread::Builder::new()
                            .name("engine-ipc-conn".to_string())
                            .spawn(move || {
                                handle_connection(stream, handle, &symbol);
                                active.fetch_sub(1, Ordering::AcqRel);
                            });
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => {
                        tracing::error!(%error, "engine IPC accept failed");
                        break;
                    }
                }
            }
            let _ = std::fs::remove_file(config.path);
        })
}

struct Pending {
    client_id: u64,
    started: Instant,
    kind: PendingKind,
}

enum PendingKind {
    Order {
        request_id: u64,
        ticket: OrderTicket,
    },
    Cancel {
        request_id: u64,
        cancel: CancelOrderRequest,
        ticket: CancelTicket,
    },
    Immediate(Vec<Vec<u8>>),
}

fn handle_connection(stream: UnixStream, handle: MatchingHandle, symbol: &str) {
    tracing::info!("engine IPC handler started");
    // The listener was set nonblocking; accepted sockets may inherit that on
    // some platforms. Engine handlers intentionally use blocking reads.
    if let Err(error) = stream.set_nonblocking(false) {
        tracing::warn!(%error, "failed to make engine IPC stream blocking");
        return;
    }
    let Ok(read_half) = stream.try_clone() else {
        tracing::warn!("engine IPC stream clone failed");
        return;
    };
    let mut read_half = BufReader::with_capacity(64 * 1024, read_half);
    let (sender, receiver) = crossbeam::channel::bounded::<Pending>(1024);
    let latency = METRICS.request_latency_ns.with_label_values(&[symbol]);
    let writer = thread::Builder::new()
        .name("engine-ipc-writer".to_string())
        .spawn(move || {
            let mut writer = stream;
            while let Ok(pending) = receiver.recv() {
                let (response, started) = build_pending(pending);
                if write_response(&mut writer, &response).is_err() {
                    break;
                }
                latency.observe(started.elapsed().as_nanos() as f64);
            }
        });

    loop {
        let request = match read_request(&mut read_half) {
            Ok(Some(request)) => request,
            Ok(None) => {
                tracing::info!("engine IPC peer closed");
                break;
            }
            Err(error) => {
                tracing::warn!(%error, "engine IPC read failed");
                break;
            }
        };
        let client_id = request.client_id;
        let started = Instant::now();
        let kind = match decode_frame_payload::<ClientRequest>(&request.payload) {
            Ok(client) if client.version != PROTOCOL_VERSION => PendingKind::Immediate(rejection(
                client.request_id,
                ErrorCode::ProtocolVersion,
                format!("unsupported protocol version {}", client.version),
            )),
            Ok(ClientRequest {
                request_id,
                message: ClientMessage::NewOrder(order),
                ..
            }) => match handle.submit_order_ticket(order) {
                Ok(ticket) => PendingKind::Order { request_id, ticket },
                Err(error) => PendingKind::Immediate(rejection(
                    request_id,
                    ErrorCode::Internal,
                    error.to_string(),
                )),
            },
            Ok(ClientRequest {
                request_id,
                message: ClientMessage::CancelOrder(cancel),
                ..
            }) => match handle.submit_cancel_ticket(cancel.clone()) {
                Ok(ticket) => PendingKind::Cancel {
                    request_id,
                    cancel,
                    ticket,
                },
                Err(error) => PendingKind::Immediate(rejection(
                    request_id,
                    ErrorCode::Internal,
                    error.to_string(),
                )),
            },
            Err(error) => {
                PendingKind::Immediate(rejection(0, ErrorCode::Internal, error.to_string()))
            }
        };
        let pending = Pending {
            client_id,
            started,
            kind,
        };
        if sender.send(pending).is_err() {
            break;
        }
    }
    tracing::info!("engine IPC handler exiting");
    drop(sender);
    if let Ok(writer) = writer {
        let _ = writer.join();
    }
}

fn build_pending(pending: Pending) -> (IpcResponse, Instant) {
    let Pending {
        client_id,
        started,
        kind,
    } = pending;
    let payloads = match kind {
        PendingKind::Order { request_id, ticket } => order_frames(request_id, ticket.wait()),
        PendingKind::Cancel {
            request_id,
            cancel,
            ticket,
        } => cancel_frames(request_id, &cancel, ticket.wait()),
        PendingKind::Immediate(payloads) => payloads,
    };
    (
        IpcResponse {
            client_id,
            payloads,
        },
        started,
    )
}

fn response_payload(request_id: u64, message: ServerMessage) -> Vec<u8> {
    encode_to_vec(&ServerResponse {
        version: PROTOCOL_VERSION,
        request_id,
        message,
    })
    .unwrap_or_default()
}

fn rejection(request_id: u64, code: ErrorCode, message: String) -> Vec<Vec<u8>> {
    vec![response_payload(
        request_id,
        ServerMessage::Rejected { code, message },
    )]
}

fn order_frames(request_id: u64, result: Result<OrderOutcome, RuntimeError>) -> Vec<Vec<u8>> {
    match result {
        Ok((trades, confirmation)) => {
            let mut frames = Vec::with_capacity(trades.len() + 1);
            for trade in trades {
                frames.push(response_payload(request_id, ServerMessage::Trade(trade)));
            }
            if let Some(confirmation) = confirmation {
                frames.push(response_payload(
                    request_id,
                    ServerMessage::Confirmation(confirmation),
                ));
            }
            frames
        }
        Err(RuntimeError::Service(ServiceError::Rejected(reason))) => {
            rejection(request_id, ErrorCode::OrderRejected, reason)
        }
        Err(error) => rejection(request_id, ErrorCode::Internal, error.to_string()),
    }
}

fn cancel_frames(
    request_id: u64,
    cancel: &CancelOrderRequest,
    result: Result<(), RuntimeError>,
) -> Vec<Vec<u8>> {
    match result {
        Ok(()) => vec![response_payload(
            request_id,
            ServerMessage::CancelAcknowledged(OrderConfirmation {
                order_id: cancel.order_id,
                user_id: cancel.user_id,
            }),
        )],
        Err(RuntimeError::Service(ServiceError::Rejected(reason))) => {
            rejection(request_id, ErrorCode::CancelRejected, reason)
        }
        Err(error) => rejection(request_id, ErrorCode::Internal, error.to_string()),
    }
}

/// Removes the engine socket path.
pub fn cleanup(path: &Path) {
    let _ = std::fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_roundtrip() {
        let request = IpcRequest {
            client_id: 42,
            payload: vec![1, 2, 3],
        };
        let encoded = encode_request(&request);
        let (decoded, consumed) = decode_request(&encoded).expect("decode request");
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded.client_id, 42);
        assert_eq!(decoded.payload, vec![1, 2, 3]);
    }

    #[test]
    fn test_response_roundtrip() {
        let response = IpcResponse {
            client_id: 7,
            payloads: vec![vec![1, 2], vec![3, 4, 5]],
        };
        let encoded = encode_response(&response);
        let (decoded, consumed) = decode_response(&encoded).expect("decode response");
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded.client_id, 7);
        assert_eq!(decoded.payloads, response.payloads);
    }
}
