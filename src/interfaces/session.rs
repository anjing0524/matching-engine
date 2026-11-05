//! Client session handling with bounded request pipelining.
//!
//! One reader thread decodes requests and enqueues order/cancel tickets; one
//! writer thread waits for the matching replies and writes responses in request
//! order. The bounded ticket channel is the per-connection in-flight limit.
use crate::application::services::{
    CancelTicket, MatchingHandle, OrderTicket, RuntimeError, ServiceError,
};
use crate::infrastructure::network::TcpConnection;
use crate::interfaces::gateway::GatewayControl;
use crate::shared::metrics::METRICS;
use crate::shared::protocol::{
    CancelOrderRequest, ClientMessage, ClientRequest, ErrorCode, OrderConfirmation,
    PROTOCOL_VERSION, ServerMessage, ServerResponse,
};
use crate::shared::wire::{WireEncode, decode};
use crossbeam::channel::{Receiver, Sender, bounded};
use prometheus::Histogram;
use std::thread;
use std::time::{Duration, Instant};

/// Errors surfaced by a session.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SessionError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Wire(#[from] crate::shared::wire::WireError),
    #[error("matching worker is unavailable")]
    Unavailable,
    #[error("fatal persistence failure: {0}")]
    Fatal(String),
}

/// Per-process session latency handles.
#[derive(Clone)]
pub(crate) struct SessionMetrics {
    request_latency_ns: Histogram,
    network_decode_ns: Histogram,
    network_encode_ns: Histogram,
    socket_send_ns: Histogram,
    socket_to_socket_ns: Histogram,
}

impl SessionMetrics {
    pub(crate) fn new(symbol: &str) -> Self {
        Self {
            request_latency_ns: METRICS.request_latency_ns.with_label_values(&[symbol]),
            network_decode_ns: METRICS.network_decode_ns.with_label_values(&[symbol]),
            network_encode_ns: METRICS.network_encode_ns.with_label_values(&[symbol]),
            socket_send_ns: METRICS.socket_send_ns.with_label_values(&[symbol]),
            socket_to_socket_ns: METRICS.socket_to_socket_ns.with_label_values(&[symbol]),
        }
    }
}

enum Pending {
    Order {
        request_id: u64,
        wire_in: Instant,
        ticket: OrderTicket,
    },
    Cancel {
        request_id: u64,
        wire_in: Instant,
        cancel: CancelOrderRequest,
        ticket: CancelTicket,
    },
    Reject {
        request_id: u64,
        message: ServerMessage,
    },
}

/// Serves one client connection until EOF, idle timeout, or shutdown.
///
/// `max_in_flight` bounds how many submitted requests may be waiting for
/// matching replies at once; the reader blocks on the ticket channel when full.
pub(crate) fn serve(
    conn: TcpConnection,
    matching: MatchingHandle,
    metrics: SessionMetrics,
    control: GatewayControl,
    idle_timeout: Option<Duration>,
    max_in_flight: usize,
) -> Result<(), SessionError> {
    if max_in_flight <= 1 {
        return serve_sync(conn, matching, metrics, control, idle_timeout);
    }
    conn.set_read_timeout(idle_timeout)?;
    let writer_conn = TcpConnection::new(conn.try_clone()?, conn.peer_addr());
    writer_conn.set_write_timeout(Some(Duration::from_secs(5)))?;

    let (pending_tx, pending_rx) = bounded(max_in_flight.max(1));
    let writer_metrics = metrics.clone();
    let writer_control = control.clone();
    let writer = thread::Builder::new()
        .name("session-writer".to_string())
        .stack_size(128 * 1024)
        .spawn(move || write_loop(writer_conn, pending_rx, writer_metrics, writer_control))?;

    let reader_result = read_loop(conn, matching, metrics, control, pending_tx);
    // Dropping the reader's sender lets the writer drain remaining replies and exit.
    let writer_result = writer.join().map_err(|_| SessionError::Unavailable)?;
    reader_result?;
    writer_result
}

/// Single-threaded session path for the low-latency default (in-flight 1).
fn serve_sync(
    mut conn: TcpConnection,
    matching: MatchingHandle,
    metrics: SessionMetrics,
    control: GatewayControl,
    idle_timeout: Option<Duration>,
) -> Result<(), SessionError> {
    conn.set_read_timeout(idle_timeout)?;
    conn.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut response_buffer = Vec::with_capacity(128);

    loop {
        if control.is_shutdown() {
            return Ok(());
        }
        let payload = match conn.recv() {
            Ok(payload) => payload,
            Err(error) if is_timeout(&error) => {
                tracing::info!("closing idle connection after timeout");
                return Ok(());
            }
            Err(error) if is_disconnect(&error) => {
                tracing::info!("client disconnected");
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };

        let wire_in = Instant::now();
        let request = {
            let started = Instant::now();
            let request = decode::<ClientRequest>(payload)?;
            metrics
                .network_decode_ns
                .observe(started.elapsed().as_nanos() as f64);
            request
        };
        if request.version != PROTOCOL_VERSION {
            send_server_message(
                &mut conn,
                &mut response_buffer,
                request.request_id,
                ServerMessage::Rejected {
                    code: ErrorCode::ProtocolVersion,
                    message: format!(
                        "unsupported protocol version {}; server uses {PROTOCOL_VERSION}",
                        request.version
                    ),
                },
                &metrics,
            )?;
            return Ok(());
        }
        let request_id = request.request_id;
        match request.message {
            ClientMessage::NewOrder(order) => {
                let started = Instant::now();
                let result = matching.submit_order(order);
                metrics
                    .request_latency_ns
                    .observe(started.elapsed().as_nanos() as f64);
                write_order_result(
                    &mut conn,
                    &mut response_buffer,
                    ReplyMeta {
                        request_id,
                        wire_in,
                    },
                    result,
                    &metrics,
                    &control,
                )?;
            }
            ClientMessage::CancelOrder(cancel) => {
                let started = Instant::now();
                let result = matching.submit_cancel(cancel.clone());
                metrics
                    .request_latency_ns
                    .observe(started.elapsed().as_nanos() as f64);
                write_cancel_result(
                    &mut conn,
                    &mut response_buffer,
                    ReplyMeta {
                        request_id,
                        wire_in,
                    },
                    &cancel,
                    result,
                    &metrics,
                    &control,
                )?;
            }
        }
    }
}

#[derive(Clone, Copy)]
struct ReplyMeta {
    request_id: u64,
    wire_in: Instant,
}

fn write_order_result(
    conn: &mut TcpConnection,
    buffer: &mut Vec<u8>,
    meta: ReplyMeta,
    result: Result<
        (
            Vec<crate::shared::protocol::TradeNotification>,
            Option<OrderConfirmation>,
        ),
        RuntimeError,
    >,
    metrics: &SessionMetrics,
    control: &GatewayControl,
) -> Result<(), SessionError> {
    match result {
        Ok((trades, confirmation)) => {
            for trade in trades {
                send_server_message(
                    conn,
                    buffer,
                    meta.request_id,
                    ServerMessage::Trade(trade),
                    metrics,
                )?;
            }
            if let Some(confirmation) = confirmation {
                send_server_message(
                    conn,
                    buffer,
                    meta.request_id,
                    ServerMessage::Confirmation(confirmation),
                    metrics,
                )?;
            }
            metrics
                .socket_to_socket_ns
                .observe(meta.wire_in.elapsed().as_nanos() as f64);
            Ok(())
        }
        Err(RuntimeError::Service(ServiceError::Rejected(reason))) => {
            send_server_message(
                conn,
                buffer,
                meta.request_id,
                ServerMessage::Rejected {
                    code: ErrorCode::OrderRejected,
                    message: reason,
                },
                metrics,
            )?;
            metrics
                .socket_to_socket_ns
                .observe(meta.wire_in.elapsed().as_nanos() as f64);
            Ok(())
        }
        Err(error) => Err(runtime_error(error, control)),
    }
}

fn write_cancel_result(
    conn: &mut TcpConnection,
    buffer: &mut Vec<u8>,
    meta: ReplyMeta,
    cancel: &CancelOrderRequest,
    result: Result<(), RuntimeError>,
    metrics: &SessionMetrics,
    control: &GatewayControl,
) -> Result<(), SessionError> {
    match result {
        Ok(()) => {
            send_server_message(
                conn,
                buffer,
                meta.request_id,
                ServerMessage::CancelAcknowledged(OrderConfirmation {
                    order_id: cancel.order_id,
                    user_id: cancel.user_id,
                }),
                metrics,
            )?;
            metrics
                .socket_to_socket_ns
                .observe(meta.wire_in.elapsed().as_nanos() as f64);
            Ok(())
        }
        Err(RuntimeError::Service(ServiceError::Rejected(reason))) => {
            send_server_message(
                conn,
                buffer,
                meta.request_id,
                ServerMessage::Rejected {
                    code: ErrorCode::CancelRejected,
                    message: reason,
                },
                metrics,
            )?;
            metrics
                .socket_to_socket_ns
                .observe(meta.wire_in.elapsed().as_nanos() as f64);
            Ok(())
        }
        Err(error) => Err(runtime_error(error, control)),
    }
}

fn read_loop(
    mut conn: TcpConnection,
    matching: MatchingHandle,
    metrics: SessionMetrics,
    control: GatewayControl,
    pending: Sender<Pending>,
) -> Result<(), SessionError> {
    loop {
        if control.is_shutdown() {
            return Ok(());
        }

        let payload = match conn.recv() {
            Ok(payload) => payload,
            Err(error) if is_timeout(&error) => {
                tracing::info!("closing idle connection after timeout");
                return Ok(());
            }
            Err(error) if is_disconnect(&error) => {
                tracing::info!("client disconnected");
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };

        let wire_in = Instant::now();
        let request = {
            let started = Instant::now();
            let request = decode::<ClientRequest>(payload)?;
            metrics
                .network_decode_ns
                .observe(started.elapsed().as_nanos() as f64);
            request
        };

        if request.version != PROTOCOL_VERSION {
            let message = ServerMessage::Rejected {
                code: ErrorCode::ProtocolVersion,
                message: format!(
                    "unsupported protocol version {}; server uses {PROTOCOL_VERSION}",
                    request.version
                ),
            };
            let _ = pending.send(Pending::Reject {
                request_id: request.request_id,
                message,
            });
            return Ok(());
        }

        match request.message {
            ClientMessage::NewOrder(order) => {
                let ticket = matching
                    .submit_order_ticket(order)
                    .map_err(|error| runtime_error(error, &control))?;
                if pending
                    .send(Pending::Order {
                        request_id: request.request_id,
                        wire_in,
                        ticket,
                    })
                    .is_err()
                {
                    return Err(SessionError::Unavailable);
                }
            }
            ClientMessage::CancelOrder(cancel) => {
                let cancel_for_ack = cancel.clone();
                let ticket = matching
                    .submit_cancel_ticket(cancel)
                    .map_err(|error| runtime_error(error, &control))?;
                if pending
                    .send(Pending::Cancel {
                        request_id: request.request_id,
                        wire_in,
                        cancel: cancel_for_ack,
                        ticket,
                    })
                    .is_err()
                {
                    return Err(SessionError::Unavailable);
                }
            }
        }
    }
}

fn write_loop(
    mut conn: TcpConnection,
    pending: Receiver<Pending>,
    metrics: SessionMetrics,
    control: GatewayControl,
) -> Result<(), SessionError> {
    let mut response_buffer = Vec::with_capacity(128);

    while let Ok(item) = pending.recv() {
        match item {
            Pending::Reject {
                request_id,
                message,
            } => {
                send_server_message(
                    &mut conn,
                    &mut response_buffer,
                    request_id,
                    message,
                    &metrics,
                )?;
            }
            Pending::Order {
                request_id,
                wire_in,
                ticket,
            } => {
                let submitted_at = ticket.submitted_at();
                let result = ticket.wait();
                metrics
                    .request_latency_ns
                    .observe(submitted_at.elapsed().as_nanos() as f64);

                match result {
                    Ok((trades, confirmation)) => {
                        for trade in trades {
                            send_server_message(
                                &mut conn,
                                &mut response_buffer,
                                request_id,
                                ServerMessage::Trade(trade),
                                &metrics,
                            )?;
                        }
                        if let Some(confirmation) = confirmation {
                            send_server_message(
                                &mut conn,
                                &mut response_buffer,
                                request_id,
                                ServerMessage::Confirmation(confirmation),
                                &metrics,
                            )?;
                        }
                        metrics
                            .socket_to_socket_ns
                            .observe(wire_in.elapsed().as_nanos() as f64);
                    }
                    Err(RuntimeError::Service(ServiceError::Rejected(reason))) => {
                        send_server_message(
                            &mut conn,
                            &mut response_buffer,
                            request_id,
                            ServerMessage::Rejected {
                                code: ErrorCode::OrderRejected,
                                message: reason,
                            },
                            &metrics,
                        )?;
                        metrics
                            .socket_to_socket_ns
                            .observe(wire_in.elapsed().as_nanos() as f64);
                    }
                    Err(error) => return Err(runtime_error(error, &control)),
                }
            }
            Pending::Cancel {
                request_id,
                wire_in,
                cancel,
                ticket,
            } => {
                let submitted_at = ticket.submitted_at();
                let result = ticket.wait();
                metrics
                    .request_latency_ns
                    .observe(submitted_at.elapsed().as_nanos() as f64);
                match result {
                    Ok(()) => {
                        send_server_message(
                            &mut conn,
                            &mut response_buffer,
                            request_id,
                            ServerMessage::CancelAcknowledged(OrderConfirmation {
                                order_id: cancel.order_id,
                                user_id: cancel.user_id,
                            }),
                            &metrics,
                        )?;
                        metrics
                            .socket_to_socket_ns
                            .observe(wire_in.elapsed().as_nanos() as f64);
                    }
                    Err(RuntimeError::Service(ServiceError::Rejected(reason))) => {
                        send_server_message(
                            &mut conn,
                            &mut response_buffer,
                            request_id,
                            ServerMessage::Rejected {
                                code: ErrorCode::OrderRejected,
                                message: reason,
                            },
                            &metrics,
                        )?;
                        metrics
                            .socket_to_socket_ns
                            .observe(wire_in.elapsed().as_nanos() as f64);
                    }
                    Err(error) => return Err(runtime_error(error, &control)),
                }
            }
        }
    }
    Ok(())
}

fn runtime_error(error: RuntimeError, control: &GatewayControl) -> SessionError {
    match error {
        RuntimeError::Service(ServiceError::Persistence(error))
        | RuntimeError::Persistence(error) => {
            control.set_fatal(error.to_string());
            SessionError::Fatal(error.to_string())
        }
        RuntimeError::Fatal(message) => {
            control.set_fatal(message.clone());
            SessionError::Fatal(message)
        }
        RuntimeError::Service(ServiceError::Rejected(reason)) => {
            SessionError::Fatal(format!("unexpected rejection: {reason}"))
        }
        RuntimeError::Closed | RuntimeError::WorkerPanicked => SessionError::Unavailable,
    }
}

fn is_timeout(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

fn is_disconnect(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::BrokenPipe
    )
}

fn send_server_message(
    conn: &mut TcpConnection,
    buffer: &mut Vec<u8>,
    request_id: u64,
    message: ServerMessage,
    metrics: &SessionMetrics,
) -> Result<(), SessionError> {
    let encode_started = Instant::now();
    buffer.clear();
    ServerResponse {
        version: PROTOCOL_VERSION,
        request_id,
        message,
    }
    .encode(buffer)?;
    metrics
        .network_encode_ns
        .observe(encode_started.elapsed().as_nanos() as f64);

    let send_started = Instant::now();
    conn.send(buffer)?;
    metrics
        .socket_send_ns
        .observe(send_started.elapsed().as_nanos() as f64);
    Ok(())
}
