//! UDP transport for public combination fills.
//!
//! Combination fills are published as fixed 64-byte packets carrying the
//! realized spread and both leg prices. Public consumers can reconstruct the
//! leg fills without member identity.

use crate::exchange::combo_engine::ComboPublicFill;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;

pub const COMBO_PUBLIC_SLOT_SIZE: usize = 64;

#[must_use]
pub fn encode_combo_public(fill: &ComboPublicFill) -> [u8; COMBO_PUBLIC_SLOT_SIZE] {
    let mut out = [0u8; COMBO_PUBLIC_SLOT_SIZE];
    out[0..8].copy_from_slice(&fill.seq.to_le_bytes());
    out[8..16].copy_from_slice(&fill.quantity.to_le_bytes());
    out[16..24].copy_from_slice(&fill.leg1_price.to_le_bytes());
    out[24..32].copy_from_slice(&fill.leg2_price.to_le_bytes());
    out[32..40].copy_from_slice(&fill.spread_ticks.to_le_bytes());
    out
}

pub fn decode_combo_public(
    packet: &[u8; COMBO_PUBLIC_SLOT_SIZE],
) -> Result<ComboPublicFill, io::Error> {
    let quantity = u64::from_le_bytes(packet[8..16].try_into().expect("8 bytes"));
    if quantity == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "combo fill quantity is zero",
        ));
    }
    Ok(ComboPublicFill {
        seq: u64::from_le_bytes(packet[0..8].try_into().expect("8 bytes")),
        quantity,
        leg1_price: u64::from_le_bytes(packet[16..24].try_into().expect("8 bytes")),
        leg2_price: u64::from_le_bytes(packet[24..32].try_into().expect("8 bytes")),
        spread_ticks: i64::from_le_bytes(packet[32..40].try_into().expect("8 bytes")),
    })
}

/// UDP publisher for combo public fills.
pub struct ComboMarketDataPublisher {
    socket: UdpSocket,
    target: SocketAddr,
}

impl ComboMarketDataPublisher {
    /// Binds a local socket and stores the destination.
    ///
    /// # Errors
    /// Returns an I/O error if binding fails.
    pub fn bind(local: &str, target: SocketAddr) -> io::Result<Self> {
        Ok(Self {
            socket: UdpSocket::bind(local)?,
            target,
        })
    }

    /// Sends one combo fill packet.
    ///
    /// # Errors
    /// Returns an I/O error from the UDP send.
    pub fn send(&self, fill: &ComboPublicFill) -> io::Result<()> {
        self.socket
            .send_to(&encode_combo_public(fill), self.target)
            .map(|_| ())
    }
}

/// UDP subscriber for combo public fills.
pub struct ComboMarketDataSubscriber {
    socket: UdpSocket,
}

impl ComboMarketDataSubscriber {
    /// Binds a local socket with a receive timeout.
    ///
    /// # Errors
    /// Returns an I/O error if binding fails.
    pub fn bind(local: &str) -> io::Result<Self> {
        let socket = UdpSocket::bind(local)?;
        socket.set_read_timeout(Some(Duration::from_secs(2)))?;
        Ok(Self { socket })
    }

    /// Receives one combo fill packet.
    ///
    /// # Errors
    /// Returns an I/O error for transport failures or invalid packets.
    pub fn recv(&self) -> io::Result<ComboPublicFill> {
        let mut buf = [0u8; COMBO_PUBLIC_SLOT_SIZE];
        let (len, _) = self.socket.recv_from(&mut buf)?;
        if len != COMBO_PUBLIC_SLOT_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid combo fill packet length",
            ));
        }
        decode_combo_public(&buf)
    }

    #[must_use]
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.socket.local_addr().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill() -> ComboPublicFill {
        ComboPublicFill {
            seq: 3,
            quantity: 5,
            leg1_price: 50_000,
            leg2_price: 100,
            spread_ticks: 49_900,
        }
    }

    #[test]
    fn combo_fill_roundtrips() {
        assert_eq!(
            decode_combo_public(&encode_combo_public(&fill())).unwrap(),
            fill()
        );
    }

    #[test]
    fn combo_fill_is_published_over_udp() {
        let subscriber = ComboMarketDataSubscriber::bind("127.0.0.1:0").unwrap();
        let publisher =
            ComboMarketDataPublisher::bind("127.0.0.1:0", subscriber.local_addr().unwrap())
                .unwrap();
        publisher.send(&fill()).unwrap();
        assert_eq!(subscriber.recv().unwrap(), fill());
    }
}
