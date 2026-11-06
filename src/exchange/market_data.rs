//! UDP market-data publisher/subscriber.
//!
//! `PublicEvent` records are fixed 128-byte packets. The same wire format is
//! used for unicast repair and multicast distribution; this slice provides the
//! UDP endpoints and packet codec, not the NACK/retransmit loop.

use crate::exchange::public_events::PublicEvent;
use crate::exchange::transport::{
    PUBLIC_EVENT_SLOT_SIZE, decode_public_event, encode_public_event,
};
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;

/// UDP sender for public events. The target may be a multicast group.
pub struct MarketDataPublisher {
    socket: UdpSocket,
    target: SocketAddr,
}

impl MarketDataPublisher {
    /// Binds a local UDP socket and stores the destination.
    ///
    /// # Errors
    /// Returns an I/O error if bind fails.
    pub fn bind(local: &str, target: SocketAddr) -> io::Result<Self> {
        let socket = UdpSocket::bind(local)?;
        Ok(Self { socket, target })
    }

    /// Binds a publisher targeting an IPv4 multicast group and port.
    ///
    /// # Errors
    /// Returns an I/O error if bind/address parsing fails.
    pub fn bind_multicast(local: &str, group: &str, port: u16) -> io::Result<Self> {
        let group: std::net::Ipv4Addr = group
            .parse()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let socket = UdpSocket::bind(local)?;
        socket.set_multicast_loop_v4(true)?;
        Ok(Self {
            socket,
            target: SocketAddr::from((group, port)),
        })
    }

    /// Sends one public event packet.
    ///
    /// # Errors
    /// Returns an I/O error from the UDP send.
    pub fn send(&self, event: &PublicEvent) -> io::Result<()> {
        self.socket
            .send_to(&encode_public_event(event), self.target)
            .map(|_| ())
    }

    #[must_use]
    pub fn target(&self) -> SocketAddr {
        self.target
    }
}

/// UDP receiver for public events.
pub struct MarketDataSubscriber {
    socket: UdpSocket,
}

impl MarketDataSubscriber {
    /// Binds a local UDP socket.
    ///
    /// # Errors
    /// Returns an I/O error if bind fails.
    pub fn bind(local: &str) -> io::Result<Self> {
        let socket = UdpSocket::bind(local)?;
        socket.set_read_timeout(Some(Duration::from_secs(2)))?;
        Ok(Self { socket })
    }

    /// Joins an IPv4 multicast group.
    ///
    /// # Errors
    /// Returns an I/O error if the group address/port is invalid or join fails.
    pub fn join_multicast(&self, group: &str) -> io::Result<()> {
        let group: std::net::Ipv4Addr = group
            .parse()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        self.socket
            .join_multicast_v4(&group, &std::net::Ipv4Addr::UNSPECIFIED)
    }

    /// Binds a subscriber on all interfaces and joins an IPv4 multicast group.
    ///
    /// # Errors
    /// Returns an I/O error if bind/join fails.
    pub fn bind_multicast(port: u16, group: &str) -> io::Result<Self> {
        let socket = UdpSocket::bind(("0.0.0.0", port))?;
        socket.set_read_timeout(Some(Duration::from_secs(2)))?;
        socket.set_multicast_loop_v4(true)?;
        let group: std::net::Ipv4Addr = group
            .parse()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        socket.join_multicast_v4(&group, &std::net::Ipv4Addr::UNSPECIFIED)?;
        Ok(Self { socket })
    }

    /// Receives one public event.
    ///
    /// # Errors
    /// Returns an I/O error for timeout/transport failures or invalid packets.
    pub fn recv(&self) -> io::Result<PublicEvent> {
        let mut buf = [0u8; PUBLIC_EVENT_SLOT_SIZE];
        let (len, _) = self.socket.recv_from(&mut buf)?;
        if len != PUBLIC_EVENT_SLOT_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid public-event packet length",
            ));
        }
        decode_public_event(&buf)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, format!("{error:?}")))
    }

    #[must_use]
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.socket.local_addr().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exchange::public_events::PublicEventType;

    fn event() -> PublicEvent {
        PublicEvent {
            seq: 7,
            event_type: PublicEventType::Trade,
            exchange_ts_ns: 99,
            phase: 3,
            instrument_id: 5,
            order_id: 11,
            trade_id: 12,
            buy_order_id: 13,
            sell_order_id: 14,
            side: Some(crate::exchange::protocol::Side::Buy),
            price_ticks: 50_000,
            quantity: 9,
            volume: 0,
        }
    }

    #[test]
    #[ignore = "requires a multicast-capable interface"]
    fn multicast_publisher_and_subscriber() {
        let group = "239.255.42.99";
        let subscriber = MarketDataSubscriber::bind_multicast(0, group).unwrap();
        let port = subscriber.local_addr().unwrap().port();
        let publisher = MarketDataPublisher::bind_multicast("0.0.0.0:0", group, port).unwrap();
        publisher.send(&event()).unwrap();
        assert_eq!(subscriber.recv().unwrap(), event());
    }

    #[test]
    fn publisher_sends_packet_to_subscriber() {
        let subscriber = MarketDataSubscriber::bind("127.0.0.1:0").unwrap();
        let target = subscriber.local_addr().unwrap();
        let publisher = MarketDataPublisher::bind("127.0.0.1:0", target).unwrap();
        publisher.send(&event()).unwrap();
        assert_eq!(subscriber.recv().unwrap(), event());
    }
}
