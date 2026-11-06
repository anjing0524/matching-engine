//! Gateway-side public sequence tracking and NACK emission.

use crate::exchange::public_events::PublicEvent;
use crate::exchange::repair::NackRequest;
use std::io;
use std::net::{SocketAddr, UdpSocket};

/// Tracks the next expected `market_data_seq` and reports gaps.
#[derive(Debug, Default)]
pub struct MarketDataTracker {
    next_seq: u64,
}

impl MarketDataTracker {
    #[must_use]
    pub fn new() -> Self {
        Self { next_seq: 1 }
    }

    /// Feeds one received event and returns a NACK when a sequence gap is seen.
    pub fn observe(&mut self, event: &PublicEvent) -> Option<NackRequest> {
        if event.seq < self.next_seq {
            return None; // duplicate or already repaired
        }
        if event.seq == self.next_seq {
            self.next_seq += 1;
            return None;
        }
        let missing = NackRequest {
            from_seq: self.next_seq,
            to_seq: event.seq - 1,
        };
        self.next_seq = event.seq + 1;
        Some(missing)
    }

    #[must_use]
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }
}

/// UDP client used by a gateway to request repairs.
pub struct NackClient {
    socket: UdpSocket,
    target: SocketAddr,
}

impl NackClient {
    /// Binds a local socket and stores the repair endpoint.
    ///
    /// # Errors
    /// Returns an I/O error if binding fails.
    pub fn bind(local: &str, target: SocketAddr) -> io::Result<Self> {
        let socket = UdpSocket::bind(local)?;
        Ok(Self { socket, target })
    }

    /// Sends one NACK request.
    ///
    /// # Errors
    /// Returns an I/O error from the UDP send.
    pub fn send(&self, request: NackRequest) -> io::Result<()> {
        self.socket
            .send_to(&request.encode(), self.target)
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exchange::market_data::{MarketDataPublisher, MarketDataSubscriber};
    use crate::exchange::public_events::PublicEventType;
    use crate::exchange::repair::RepairServer;

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
    fn tracker_reports_gap() {
        let mut tracker = MarketDataTracker::new();
        assert_eq!(tracker.observe(&event(1)), None);
        assert_eq!(
            tracker.observe(&event(3)),
            Some(NackRequest {
                from_seq: 2,
                to_seq: 2
            })
        );
        assert_eq!(tracker.next_seq(), 4);
        assert_eq!(tracker.observe(&event(2)), None);
    }

    #[test]
    fn gateway_nack_triggers_repair() {
        let subscriber = MarketDataSubscriber::bind("127.0.0.1:0").unwrap();
        let publisher =
            MarketDataPublisher::bind("127.0.0.1:0", subscriber.local_addr().unwrap()).unwrap();
        let mut server = RepairServer::bind("127.0.0.1:0", publisher, 8).unwrap();
        server.push(event(2));
        let tracker = MarketDataTracker::new();
        let client = NackClient::bind("127.0.0.1:0", server.local_addr().unwrap()).unwrap();
        client
            .send(NackRequest {
                from_seq: 2,
                to_seq: 2,
            })
            .unwrap();
        let mut repaired = 0;
        for _ in 0..100 {
            repaired = server.poll_once().unwrap();
            if repaired == 1 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(repaired, 1);
        assert_eq!(subscriber.recv().unwrap().seq, 2);
        assert_eq!(tracker.next_seq(), 1);
    }
}
