//! NACK-driven public market-data repair.
//!
//! Gateways detect a `market_data_seq` gap, send a fixed NACK datagram to the
//! repair endpoint, and middleware replays the requested events through the UDP
//! publisher. This slice keeps a bounded in-memory replay log; persistent
//! full-day storage is a later layer.

use crate::exchange::market_data::MarketDataPublisher;
use crate::exchange::public_events::PublicEvent;
use std::io;
use std::net::{SocketAddr, UdpSocket};

const NACK_MAGIC: u32 = 0x4e41_434b;

/// One repair request: inclusive sequence range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NackRequest {
    pub from_seq: u64,
    pub to_seq: u64,
}

impl NackRequest {
    #[must_use]
    pub fn encode(self) -> [u8; 20] {
        let mut out = [0u8; 20];
        out[0..4].copy_from_slice(&NACK_MAGIC.to_le_bytes());
        out[4..12].copy_from_slice(&self.from_seq.to_le_bytes());
        out[12..20].copy_from_slice(&self.to_seq.to_le_bytes());
        out
    }

    pub fn decode(packet: &[u8]) -> Result<Self, io::Error> {
        if packet.len() != 20 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid NACK length",
            ));
        }
        let magic = u32::from_le_bytes(packet[0..4].try_into().expect("4 bytes"));
        if magic != NACK_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid NACK magic",
            ));
        }
        Ok(Self {
            from_seq: u64::from_le_bytes(packet[4..12].try_into().expect("8 bytes")),
            to_seq: u64::from_le_bytes(packet[12..20].try_into().expect("8 bytes")),
        })
    }
}

/// In-memory sequenced replay log; bounded by its configured capacity.
#[derive(Debug, Default)]
pub struct RepairBuffer {
    events: Vec<PublicEvent>,
    capacity: usize,
}

impl RepairBuffer {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            events: Vec::new(),
            capacity,
        }
    }

    pub fn push(&mut self, event: PublicEvent) {
        if self.capacity == 0 {
            return;
        }
        if self.events.len() == self.capacity {
            self.events.remove(0);
        }
        self.events.push(event);
    }

    #[must_use]
    pub fn repair(&self, request: NackRequest) -> Vec<PublicEvent> {
        self.events
            .iter()
            .filter(|event| event.seq >= request.from_seq && event.seq <= request.to_seq)
            .copied()
            .collect()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

/// UDP repair endpoint.
pub struct RepairServer {
    socket: UdpSocket,
    publisher: MarketDataPublisher,
    buffer: RepairBuffer,
}

impl RepairServer {
    /// Binds the NACK endpoint.
    ///
    /// # Errors
    /// Returns an I/O error if binding fails.
    pub fn bind(local: &str, publisher: MarketDataPublisher, capacity: usize) -> io::Result<Self> {
        let socket = UdpSocket::bind(local)?;
        socket.set_read_timeout(Some(std::time::Duration::from_secs(2)))?;
        Ok(Self {
            socket,
            publisher,
            buffer: RepairBuffer::new(capacity),
        })
    }

    pub fn push(&mut self, event: PublicEvent) {
        self.buffer.push(event);
    }

    #[must_use]
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.socket.local_addr().ok()
    }

    /// Receives one NACK and replays the requested range.
    ///
    /// # Errors
    /// Returns an I/O error for receive/decode/publish failures.
    pub fn poll_once(&self) -> io::Result<usize> {
        let mut packet = [0u8; 64];
        let (len, _) = self.socket.recv_from(&mut packet)?;
        let request = NackRequest::decode(&packet[..len])?;
        let events = self.buffer.repair(request);
        for event in &events {
            self.publisher.send(event)?;
        }
        Ok(events.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exchange::market_data::MarketDataSubscriber;
    use crate::exchange::public_events::PublicEventType;

    fn event(seq: u64) -> PublicEvent {
        PublicEvent {
            seq,
            event_type: PublicEventType::Trade,
            exchange_ts_ns: 0,
            phase: 3,
            instrument_id: 1,
            order_id: 0,
            trade_id: seq,
            buy_order_id: 0,
            sell_order_id: 0,
            side: None,
            price_ticks: 50_000,
            quantity: 1,
            volume: 0,
        }
    }

    #[test]
    fn nack_roundtrip() {
        let request = NackRequest {
            from_seq: 2,
            to_seq: 9,
        };
        assert_eq!(NackRequest::decode(&request.encode()).unwrap(), request);
    }

    #[test]
    fn repair_server_replays_requested_range() {
        let subscriber = MarketDataSubscriber::bind("127.0.0.1:0").unwrap();
        let publisher =
            MarketDataPublisher::bind("127.0.0.1:0", subscriber.local_addr().unwrap()).unwrap();
        let mut server = RepairServer::bind("127.0.0.1:0", publisher, 16).unwrap();
        server.push(event(1));
        server.push(event(2));
        server.push(event(3));

        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client
            .send_to(
                &NackRequest {
                    from_seq: 2,
                    to_seq: 3,
                }
                .encode(),
                server.local_addr().unwrap(),
            )
            .unwrap();
        assert_eq!(server.poll_once().unwrap(), 2);
        assert_eq!(subscriber.recv().unwrap().seq, 2);
        assert_eq!(subscriber.recv().unwrap().seq, 3);
    }
}
