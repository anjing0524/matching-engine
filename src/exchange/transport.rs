//! Shared-memory command transport between middleware and core.
//!
//! One file-backed SPSC ring carries fixed 256-byte command slots. The producer
//! is the middleware sequencer; the consumer is the core. No syscalls are made
//! on the hot path after the mapping is established.

use crate::exchange::protocol::{COMMAND_SLOT_SIZE, Command, ProtocolError, Side};
use crate::exchange::public_events::{PublicEvent, PublicEventType};
use crate::shared::spsc_ring::{PushError, SpscRing};
use std::io;
use std::path::Path;

/// Producer side of the command ring.
pub struct CommandProducer {
    ring: SpscRing,
}

/// Consumer side of the command ring.
pub struct CommandConsumer {
    ring: SpscRing,
}

#[derive(Debug)]
pub enum CommandTransportError {
    Full,
    Decode(ProtocolError),
    Io(io::Error),
}

impl From<io::Error> for CommandTransportError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Creates a file-backed command ring and returns both endpoints.
///
/// # Errors
/// Returns an I/O error if the ring file cannot be created or opened.
pub fn create(path: &Path, capacity: u64) -> io::Result<(CommandProducer, CommandConsumer)> {
    let producer = SpscRing::create(path, capacity, COMMAND_SLOT_SIZE)?;
    let consumer = SpscRing::open(path)?;
    Ok((
        CommandProducer { ring: producer },
        CommandConsumer { ring: consumer },
    ))
}

impl CommandProducer {
    /// Appends one command. `Full` is an explicit backpressure signal.
    pub fn send(&self, command: &Command) -> Result<(), CommandTransportError> {
        let bytes = command.encode();
        self.ring.try_push(&bytes).map_err(|error| match error {
            PushError::Full => CommandTransportError::Full,
            PushError::TooLarge => CommandTransportError::Decode(ProtocolError {
                field: "slot_size",
                value: u8::MAX,
            }),
        })
    }

    #[must_use]
    pub fn capacity(&self) -> u64 {
        self.ring.capacity()
    }

    #[must_use]
    pub fn len(&self) -> u64 {
        self.ring.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }
}

impl CommandConsumer {
    /// Pops one command; `None` means the ring is empty.
    pub fn recv(&self) -> Option<Result<Command, ProtocolError>> {
        let mut bytes = Vec::with_capacity(COMMAND_SLOT_SIZE);
        if !self.ring.try_pop_into(&mut bytes) {
            return None;
        }
        if bytes.len() != COMMAND_SLOT_SIZE {
            return Some(Err(ProtocolError {
                field: "command_slot",
                value: bytes.len() as u8,
            }));
        }
        let mut slot = [0u8; COMMAND_SLOT_SIZE];
        slot.copy_from_slice(&bytes);
        Some(Command::decode(&slot))
    }

    #[must_use]
    pub fn capacity(&self) -> u64 {
        self.ring.capacity()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }
}

/// Fixed slot size for public event transport.
pub const PUBLIC_EVENT_SLOT_SIZE: usize = 128;
const PUBLIC_EVENT_MAGIC: u32 = 0x5045_5631;

/// Producer side of the public-event ring.
pub struct PublicEventProducer {
    ring: SpscRing,
}

/// Consumer side of the public-event ring.
pub struct PublicEventConsumer {
    ring: SpscRing,
}

/// Creates a file-backed public-event ring.
///
/// # Errors
/// Returns an I/O error if the ring file cannot be created or opened.
pub fn create_public_event_transport(
    path: &Path,
    capacity: u64,
) -> io::Result<(PublicEventProducer, PublicEventConsumer)> {
    let producer = SpscRing::create(path, capacity, PUBLIC_EVENT_SLOT_SIZE)?;
    let consumer = SpscRing::open(path)?;
    Ok((
        PublicEventProducer { ring: producer },
        PublicEventConsumer { ring: consumer },
    ))
}

impl PublicEventProducer {
    /// Appends one public event.
    pub fn send(&self, event: &PublicEvent) -> Result<(), CommandTransportError> {
        let bytes = encode_public_event(event);
        self.ring.try_push(&bytes).map_err(|error| match error {
            PushError::Full => CommandTransportError::Full,
            PushError::TooLarge => CommandTransportError::Decode(ProtocolError {
                field: "public_slot",
                value: u8::MAX,
            }),
        })
    }
}

impl PublicEventConsumer {
    /// Pops one public event; `None` means empty.
    pub fn recv(&self) -> Option<Result<PublicEvent, ProtocolError>> {
        let mut bytes = Vec::with_capacity(PUBLIC_EVENT_SLOT_SIZE);
        if !self.ring.try_pop_into(&mut bytes) {
            return None;
        }
        let mut slot = [0u8; PUBLIC_EVENT_SLOT_SIZE];
        if bytes.len() != PUBLIC_EVENT_SLOT_SIZE {
            return Some(Err(ProtocolError {
                field: "public_slot",
                value: bytes.len() as u8,
            }));
        }
        slot.copy_from_slice(&bytes);
        Some(decode_public_event(&slot))
    }
}

fn encode_public_event(event: &PublicEvent) -> [u8; PUBLIC_EVENT_SLOT_SIZE] {
    let mut out = [0u8; PUBLIC_EVENT_SLOT_SIZE];
    out[0..4].copy_from_slice(&PUBLIC_EVENT_MAGIC.to_le_bytes());
    out[4..12].copy_from_slice(&event.seq.to_le_bytes());
    out[12] = match event.event_type {
        PublicEventType::TimeTick => 1,
        PublicEventType::OrderRested => 2,
        PublicEventType::OrderCanceled => 3,
        PublicEventType::Trade => 4,
        PublicEventType::AuctionResult => 5,
        PublicEventType::SessionSwitch => 6,
    };
    out[13] = event.phase;
    out[14] = match event.side {
        None => 0,
        Some(Side::Buy) => 1,
        Some(Side::Sell) => 2,
    };
    out[16..24].copy_from_slice(&event.exchange_ts_ns.to_le_bytes());
    out[24..28].copy_from_slice(&event.instrument_id.to_le_bytes());
    out[32..40].copy_from_slice(&event.order_id.to_le_bytes());
    out[40..48].copy_from_slice(&event.trade_id.to_le_bytes());
    out[48..56].copy_from_slice(&event.buy_order_id.to_le_bytes());
    out[56..64].copy_from_slice(&event.sell_order_id.to_le_bytes());
    out[64..72].copy_from_slice(&event.price_ticks.to_le_bytes());
    out[72..80].copy_from_slice(&event.quantity.to_le_bytes());
    out[80..88].copy_from_slice(&event.volume.to_le_bytes());
    out
}

fn decode_public_event(slot: &[u8; PUBLIC_EVENT_SLOT_SIZE]) -> Result<PublicEvent, ProtocolError> {
    let magic = u32::from_le_bytes(slot[0..4].try_into().expect("4 bytes"));
    if magic != PUBLIC_EVENT_MAGIC {
        return Err(ProtocolError {
            field: "public_magic",
            value: slot[0],
        });
    }
    let event_type = match slot[12] {
        1 => PublicEventType::TimeTick,
        2 => PublicEventType::OrderRested,
        3 => PublicEventType::OrderCanceled,
        4 => PublicEventType::Trade,
        5 => PublicEventType::AuctionResult,
        6 => PublicEventType::SessionSwitch,
        value => {
            return Err(ProtocolError {
                field: "public_event_type",
                value,
            });
        }
    };
    let side = match slot[14] {
        0 => None,
        1 => Some(Side::Buy),
        2 => Some(Side::Sell),
        value => {
            return Err(ProtocolError {
                field: "public_side",
                value,
            });
        }
    };
    Ok(PublicEvent {
        seq: u64::from_le_bytes(slot[4..12].try_into().expect("8 bytes")),
        event_type,
        exchange_ts_ns: u64::from_le_bytes(slot[16..24].try_into().expect("8 bytes")),
        phase: slot[13],
        instrument_id: u32::from_le_bytes(slot[24..28].try_into().expect("4 bytes")),
        order_id: u64::from_le_bytes(slot[32..40].try_into().expect("8 bytes")),
        trade_id: u64::from_le_bytes(slot[40..48].try_into().expect("8 bytes")),
        buy_order_id: u64::from_le_bytes(slot[48..56].try_into().expect("8 bytes")),
        sell_order_id: u64::from_le_bytes(slot[56..64].try_into().expect("8 bytes")),
        side,
        price_ticks: u64::from_le_bytes(slot[64..72].try_into().expect("8 bytes")),
        quantity: u64::from_le_bytes(slot[72..80].try_into().expect("8 bytes")),
        volume: u64::from_le_bytes(slot[80..88].try_into().expect("8 bytes")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exchange::protocol::{
        CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType, VolumeCondition,
    };
    use crate::exchange::public_events::{PublicEvent, PublicEventType};

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("matching-engine-{name}-{}", std::process::id()))
    }

    fn command(seq: u64) -> Command {
        Command {
            global_seq: seq,
            gateway_seq: 1,
            ingress_ts_ns: 0,
            exchange_ts_ns: 0,
            client_order_id: 1,
            session_id: 1,
            gateway_id: 1,
            member_id: 1,
            user_id: 1,
            seat_id: 1,
            instrument_id: 1,
            combo_id: 0,
            session_epoch: 1,
            command_type: CommandType::NewOrder,
            price_type: PriceType::Limit,
            time_condition: TimeCondition::Gfd,
            volume_condition: VolumeCondition::Any,
            side: Side::Buy,
            offset: Offset::Open,
            hedge: Hedge::Spec,
            flags: 0,
            leg2_ratio: 0,
            price_ticks: 50_000,
            quantity: 1,
            trigger_price_ticks: 0,
            leg2_instrument_id: 0,
            leg2_offset: Offset::Open,
            leg2_hedge: Hedge::Spec,
            trigger_type: TriggerType::None,
            cancel_order_id: 0,
        }
    }

    #[test]
    fn commands_roundtrip_through_shared_ring() {
        let path = temp_path("command-ring");
        let _ = std::fs::remove_file(&path);
        let (producer, consumer) = create(&path, 8).unwrap();
        producer.send(&command(7)).unwrap();
        let received = consumer.recv().unwrap().unwrap();
        assert_eq!(received.global_seq, 7);
        assert!(consumer.recv().is_none());
        drop(producer);
        drop(consumer);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn public_event_roundtrips_through_shared_ring() {
        let path = temp_path("public-events");
        let _ = std::fs::remove_file(&path);
        let (producer, consumer) = create_public_event_transport(&path, 8).unwrap();
        let event = PublicEvent {
            seq: 7,
            event_type: PublicEventType::Trade,
            exchange_ts_ns: 99,
            phase: 3,
            instrument_id: 5,
            order_id: 11,
            trade_id: 12,
            buy_order_id: 13,
            sell_order_id: 14,
            side: Some(Side::Buy),
            price_ticks: 50_000,
            quantity: 9,
            volume: 0,
        };
        producer.send(&event).unwrap();
        assert_eq!(consumer.recv().unwrap().unwrap(), event);
        assert!(consumer.recv().is_none());
        drop(producer);
        drop(consumer);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn full_ring_is_explicit_backpressure() {
        let path = temp_path("command-full");
        let _ = std::fs::remove_file(&path);
        let (producer, consumer) = create(&path, 2).unwrap();
        producer.send(&command(1)).unwrap();
        producer.send(&command(2)).unwrap();
        assert!(matches!(
            producer.send(&command(3)),
            Err(CommandTransportError::Full)
        ));
        assert!(consumer.recv().is_some());
        producer.send(&command(3)).unwrap();
        drop(producer);
        drop(consumer);
        let _ = std::fs::remove_file(&path);
    }
}
