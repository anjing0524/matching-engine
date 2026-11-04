//! Blocking network server example.
//!
//! Wires the blocking TCP transport to a tick-based order book. Matching is
//! protected by a mutex only for this example; production uses a dedicated
//! matching thread (see the CLI).
use matching_engine::domain::orderbook::{ContractSpec, TickBasedOrderBook};
use matching_engine::infrastructure::network::{TcpConnection, TcpTransport};
use matching_engine::shared::protocol::{
    ClientMessage, ClientRequest, PROTOCOL_VERSION, ServerMessage, ServerResponse,
};
use matching_engine::shared::wire::{WireEncode, WireError, decode};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
enum ExampleError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Wire(#[from] WireError),
    #[error(transparent)]
    Address(#[from] std::net::AddrParseError),
}

fn main() -> Result<(), ExampleError> {
    tracing_subscriber::fmt::init();

    let spec = ContractSpec {
        symbol: "BTCUSDT".to_string(),
        tick_size: 1,
        min_price: 40000,
        max_price: 70000,
        queue_capacity: 10_000,
    };
    let orderbook = Arc::new(Mutex::new(TickBasedOrderBook::new(spec)));

    let mut transport = TcpTransport::new()?;
    transport.bind("0.0.0.0:8080".parse()?)?;
    let listen_addr = transport.local_addr()?;
    tracing::info!(%listen_addr, "matching-engine server listening");

    loop {
        match transport.accept() {
            Ok(conn) => {
                tracing::info!(peer = ?conn.peer_addr(), "accepted connection");
                let orderbook = Arc::clone(&orderbook);
                thread::spawn(move || {
                    if let Err(error) = handle_connection(conn, orderbook) {
                        tracing::error!(%error, "connection handler failed");
                    }
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(1));
            }
            Err(error) => tracing::error!(%error, "accept failed"),
        }
    }
}

/// Handles one client connection until it closes.
fn handle_connection(
    mut conn: TcpConnection,
    orderbook: Arc<Mutex<TickBasedOrderBook>>,
) -> Result<(), ExampleError> {
    let mut response_buffer = Vec::with_capacity(128);
    loop {
        let payload = match conn.recv() {
            Ok(payload) => payload,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset
                ) =>
            {
                tracing::info!("client disconnected");
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };

        let request = decode::<ClientRequest>(payload)?;
        if request.version != PROTOCOL_VERSION {
            send(
                &mut conn,
                &mut response_buffer,
                request.request_id,
                ServerMessage::Rejected {
                    code: matching_engine::shared::protocol::ErrorCode::ProtocolVersion,
                    message: format!("unsupported protocol version {}", request.version),
                },
            )?;
            return Ok(());
        }
        let request_id = request.request_id;
        match request.message {
            ClientMessage::NewOrder(order) => {
                let (trades, confirmation) = {
                    let mut book = orderbook
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    book.match_order(order).into_parts()
                };

                for trade in trades {
                    send(
                        &mut conn,
                        &mut response_buffer,
                        request_id,
                        ServerMessage::Trade(trade),
                    )?;
                }
                if let Some(confirmation) = confirmation {
                    send(
                        &mut conn,
                        &mut response_buffer,
                        request_id,
                        ServerMessage::Confirmation(confirmation),
                    )?;
                }
            }
            ClientMessage::CancelOrder(cancel) => {
                let result = {
                    let mut book = orderbook
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    book.cancel_order(cancel.order_id, cancel.user_id)
                };
                if let Err(error) = result {
                    tracing::warn!(%error, "cancel rejected");
                }
            }
        }
    }
}

/// Encodes and sends one [`ServerMessage`] frame.
fn send(
    conn: &mut TcpConnection,
    buffer: &mut Vec<u8>,
    request_id: u64,
    message: ServerMessage,
) -> Result<(), ExampleError> {
    buffer.clear();
    ServerResponse {
        version: PROTOCOL_VERSION,
        request_id,
        message,
    }
    .encode(buffer)?;
    conn.send(buffer)?;
    Ok(())
}
