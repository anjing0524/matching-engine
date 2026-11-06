//! Shared-memory command transport between middleware and core.
//!
//! One file-backed SPSC ring carries fixed 256-byte command slots. The producer
//! is the middleware sequencer; the consumer is the core. No syscalls are made
//! on the hot path after the mapping is established.

use crate::exchange::protocol::{COMMAND_SLOT_SIZE, Command, ProtocolError};
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exchange::protocol::{
        CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType, VolumeCondition,
    };

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
