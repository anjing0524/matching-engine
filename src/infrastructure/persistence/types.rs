//! Persistence data types and WAL entry encoding.
use crate::shared::protocol::{
    CancelOrderRequest, NewOrderRequest, OrderConfirmation, TradeNotification,
};
use crate::shared::wire::{WireDecode, WireEncode, WireError, WireReader, WireWrite};
use std::path::PathBuf;

/// Payload of one WAL record.
#[derive(Debug, Clone)]
pub enum WalEntry {
    /// A new order was accepted.
    NewOrder(NewOrderRequest),
    /// A cancellation was accepted.
    CancelOrder(CancelOrderRequest),
    /// A trade was produced by matching.
    Trade(TradeNotification),
    /// An order rested in the book and was confirmed.
    Confirmation(OrderConfirmation),
}

impl WireEncode for WalEntry {
    fn encode(&self, out: &mut Vec<u8>) -> Result<(), WireError> {
        match self {
            Self::NewOrder(order) => {
                out.put_u8(0);
                order.encode(out)
            }
            Self::CancelOrder(cancel) => {
                out.put_u8(1);
                cancel.encode(out)
            }
            Self::Trade(trade) => {
                out.put_u8(2);
                trade.encode(out)
            }
            Self::Confirmation(confirmation) => {
                out.put_u8(3);
                confirmation.encode(out)
            }
        }
    }
}

impl WireDecode for WalEntry {
    fn decode(reader: &mut WireReader<'_>) -> Result<Self, WireError> {
        match reader.read_u8()? {
            0 => Ok(Self::NewOrder(NewOrderRequest::decode(reader)?)),
            1 => Ok(Self::CancelOrder(CancelOrderRequest::decode(reader)?)),
            2 => Ok(Self::Trade(TradeNotification::decode(reader)?)),
            3 => Ok(Self::Confirmation(OrderConfirmation::decode(reader)?)),
            tag => Err(WireError::InvalidTag {
                type_name: "WalEntry",
                tag,
            }),
        }
    }
}

/// A WAL record: sequence number and timestamp live in the record frame, the
/// [`WalEntry`] is the encoded payload.
#[derive(Debug, Clone)]
pub struct WalRecord {
    /// Monotonically increasing sequence number.
    pub seq: u64,
    /// Wall-clock timestamp in nanoseconds.
    pub timestamp: u64,
    /// Entry payload.
    pub entry: WalEntry,
}

/// Persistence configuration.
#[derive(Debug, Clone)]
pub struct PersistenceConfig {
    /// Directory holding WAL files.
    pub data_dir: PathBuf,

    /// Maximum WAL file size in bytes before rotating.
    pub wal_max_size: usize,

    /// Whether to fsync the WAL before acknowledging an order.
    ///
    /// `true` makes acknowledgements durable across power loss; `false` keeps
    /// data in the OS page cache for higher throughput.
    pub wal_sync: bool,

    /// Maximum number of client order ids remembered for idempotency.
    /// `0` disables in-memory duplicate detection.
    pub dedup_cache_size: usize,

    /// Take a snapshot after this many WAL records (0 disables snapshots).
    pub snapshot_every_records: u64,
}

impl Default for PersistenceConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("./data"),
            wal_max_size: 64 * 1024 * 1024,
            wal_sync: true,
            dedup_cache_size: 262_144,
            snapshot_every_records: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::protocol::OrderType;
    use crate::shared::wire::{decode, encode_to_vec};

    #[test]
    fn test_wal_entry_roundtrip() {
        let entries = [
            WalEntry::NewOrder(NewOrderRequest {
                user_id: 1,
                client_order_id: 0,
                symbol: crate::shared::SymbolId::intern("BTCUSDT"),
                order_type: OrderType::Buy,
                price: 50_000,
                quantity: 10,
            }),
            WalEntry::CancelOrder(CancelOrderRequest {
                user_id: 1,
                order_id: 2,
            }),
            WalEntry::Confirmation(OrderConfirmation {
                order_id: 2,
                user_id: 1,
            }),
        ];

        for entry in entries {
            let encoded = encode_to_vec(&entry).expect("failed to encode");
            let decoded: WalEntry = decode(&encoded).expect("failed to decode");
            assert_eq!(
                std::mem::discriminant(&entry),
                std::mem::discriminant(&decoded)
            );
        }
    }

    #[test]
    fn test_invalid_wal_entry_tag() {
        let error = decode::<WalEntry>(&[9]).unwrap_err();
        assert!(matches!(
            error,
            WireError::InvalidTag {
                type_name: "WalEntry",
                tag: 9
            }
        ));
    }
}
