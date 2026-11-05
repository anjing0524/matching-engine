//! Order-book snapshot format.
//!
//! A snapshot is the durable state of the book at a WAL sequence: resting
//! orders, next ids, and the idempotency keys seen so far. The file is written
//! atomically (temp + fsync + rename + directory fsync) and CRC checked.
use super::{PersistenceError, Result};
use crate::domain::orderbook::{ContractSpec, RestingOrder};
use crate::shared::protocol::OrderType;
use crate::shared::wire::{
    WireDecode, WireEncode, WireError, WireReader, WireWrite, decode, encode_to_vec,
};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

const SNAPSHOT_MAGIC: u32 = 0x534E_4150; // "SNAP"
const SNAPSHOT_VERSION: u16 = 1;
const HEADER_LEN: u64 = 4 + 2 + 2 + 4 + 4;

/// Durable snapshot payload.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub contract: ContractSpec,
    pub last_seq: u64,
    pub next_order_id: u64,
    pub next_trade_id: u64,
    pub orders: Vec<RestingOrder>,
    pub dedup_keys: Vec<(u64, u64)>,
}

impl WireEncode for Snapshot {
    fn encode(&self, out: &mut Vec<u8>) -> std::result::Result<(), WireError> {
        out.put_u64(self.last_seq);
        out.put_u64(self.next_order_id);
        out.put_u64(self.next_trade_id);
        out.put_str(&self.contract.symbol)?;
        out.put_u64(self.contract.tick_size);
        out.put_u64(self.contract.min_price);
        out.put_u64(self.contract.max_price);
        out.put_u64(self.contract.queue_capacity as u64);
        out.put_u32(self.orders.len() as u32);
        for order in &self.orders {
            out.put_u64(order.user_id);
            out.put_u64(order.order_id);
            out.put_u8(match order.side {
                OrderType::Buy => 0,
                OrderType::Sell => 1,
            });
            out.put_u64(order.price);
            out.put_u64(order.quantity);
            out.put_u64(order.original_quantity);
        }
        out.put_u32(self.dedup_keys.len() as u32);
        for (user_id, client_order_id) in &self.dedup_keys {
            out.put_u64(*user_id);
            out.put_u64(*client_order_id);
        }
        Ok(())
    }
}

impl WireDecode for Snapshot {
    fn decode(reader: &mut WireReader<'_>) -> std::result::Result<Self, WireError> {
        let last_seq = reader.read_u64()?;
        let next_order_id = reader.read_u64()?;
        let next_trade_id = reader.read_u64()?;
        let symbol = reader.read_str()?.to_string();
        let tick_size = reader.read_u64()?;
        let min_price = reader.read_u64()?;
        let max_price = reader.read_u64()?;
        let queue_capacity = reader.read_u64()? as usize;
        let order_count = reader.read_u32()? as usize;
        let mut orders = Vec::with_capacity(order_count);
        for _ in 0..order_count {
            let user_id = reader.read_u64()?;
            let order_id = reader.read_u64()?;
            let side = match reader.read_u8()? {
                0 => OrderType::Buy,
                1 => OrderType::Sell,
                tag => {
                    return Err(WireError::InvalidTag {
                        type_name: "OrderType",
                        tag,
                    });
                }
            };
            let price = reader.read_u64()?;
            let quantity = reader.read_u64()?;
            let original_quantity = reader.read_u64()?;
            orders.push(RestingOrder {
                user_id,
                order_id,
                side,
                price,
                quantity,
                original_quantity,
            });
        }
        let dedup_count = reader.read_u32()? as usize;
        let mut dedup_keys = Vec::with_capacity(dedup_count);
        for _ in 0..dedup_count {
            dedup_keys.push((reader.read_u64()?, reader.read_u64()?));
        }
        Ok(Self {
            contract: ContractSpec {
                symbol,
                tick_size,
                min_price,
                max_price,
                queue_capacity,
            },
            last_seq,
            next_order_id,
            next_trade_id,
            orders,
            dedup_keys,
        })
    }
}

impl Snapshot {
    /// Loads the snapshot if present.
    ///
    /// # Errors
    /// Returns an error if the file exists but is corrupt or has an unknown
    /// version. A corrupt snapshot is fatal; silently replaying from an older
    /// state would break the durability contract.
    pub fn load(data_dir: &Path) -> Result<Option<Self>> {
        let path = snapshot_path(data_dir);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if bytes.len() < HEADER_LEN as usize {
            return Err(PersistenceError::Corruption(format!(
                "snapshot {} is truncated",
                path.display()
            )));
        }
        let magic = u32::from_le_bytes(bytes[0..4].try_into().expect("header"));
        let version = u16::from_le_bytes(bytes[4..6].try_into().expect("header"));
        if magic != SNAPSHOT_MAGIC || version != SNAPSHOT_VERSION {
            return Err(PersistenceError::Corruption(format!(
                "snapshot {} has unsupported header",
                path.display()
            )));
        }
        let len = u32::from_le_bytes(bytes[8..12].try_into().expect("header")) as usize;
        let expected_crc = u32::from_le_bytes(bytes[12..16].try_into().expect("header"));
        if bytes.len() != (HEADER_LEN as usize + len) {
            return Err(PersistenceError::Corruption(format!(
                "snapshot {} length mismatch",
                path.display()
            )));
        }
        let payload = &bytes[HEADER_LEN as usize..];
        if crc32fast::hash(payload) != expected_crc {
            return Err(PersistenceError::Corruption(format!(
                "snapshot {} CRC mismatch",
                path.display()
            )));
        }
        let snapshot = decode::<Snapshot>(payload)
            .map_err(|error| PersistenceError::Deserialization(error.to_string()))?;
        Ok(Some(snapshot))
    }

    /// Writes the snapshot atomically.
    ///
    /// # Errors
    /// Returns an error if the payload cannot be encoded or persisted.
    pub fn save(data_dir: &Path, snapshot: &Snapshot) -> Result<()> {
        std::fs::create_dir_all(data_dir)?;
        let payload = encode_to_vec(snapshot)
            .map_err(|error| PersistenceError::Serialization(error.to_string()))?;
        if payload.len() > u32::MAX as usize {
            return Err(PersistenceError::Serialization(
                "snapshot payload exceeds u32::MAX".to_string(),
            ));
        }
        let final_path = snapshot_path(data_dir);
        let temp_path = data_dir.join(".snapshot.tmp");

        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&temp_path)?;
        file.write_all(&SNAPSHOT_MAGIC.to_le_bytes())?;
        file.write_all(&SNAPSHOT_VERSION.to_le_bytes())?;
        file.write_all(&[0u8; 2])?;
        file.write_all(&(payload.len() as u32).to_le_bytes())?;
        file.write_all(&crc32fast::hash(&payload).to_le_bytes())?;
        file.write_all(&payload)?;
        file.flush()?;
        file.sync_all()?;
        std::fs::rename(&temp_path, &final_path)?;
        fsync_dir(data_dir)?;
        Ok(())
    }
}

pub(crate) fn snapshot_path(data_dir: &Path) -> PathBuf {
    data_dir.join("snapshot.bin")
}

fn fsync_dir(dir: &Path) -> Result<()> {
    let file = File::open(dir)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn spec() -> ContractSpec {
        ContractSpec::new("BTCUSDT", 1, 40_000, 70_000)
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            contract: spec(),
            last_seq: 42,
            next_order_id: 7,
            next_trade_id: 3,
            orders: vec![RestingOrder {
                user_id: 1,
                order_id: 5,
                side: OrderType::Buy,
                price: 50_000,
                quantity: 4,
                original_quantity: 4,
            }],
            dedup_keys: vec![(1, 99)],
        }
    }

    #[test]
    fn test_snapshot_roundtrip() {
        let dir = TempDir::new().unwrap();
        let snap = snapshot();
        Snapshot::save(dir.path(), &snap).unwrap();
        let loaded = Snapshot::load(dir.path()).unwrap().expect("snapshot");
        assert_eq!(loaded.last_seq, 42);
        assert_eq!(loaded.next_trade_id, 3);
        assert_eq!(loaded.orders.len(), 1);
        assert_eq!(loaded.orders[0].order_id, 5);
        assert_eq!(loaded.dedup_keys, vec![(1, 99)]);
    }

    #[test]
    fn test_missing_snapshot_is_none() {
        let dir = TempDir::new().unwrap();
        assert!(Snapshot::load(dir.path()).unwrap().is_none());
    }

    #[test]
    fn test_corrupt_snapshot_is_rejected() {
        let dir = TempDir::new().unwrap();
        Snapshot::save(dir.path(), &snapshot()).unwrap();
        let path = snapshot_path(dir.path());
        let mut bytes = std::fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        std::fs::write(&path, bytes).unwrap();
        assert!(matches!(
            Snapshot::load(dir.path()),
            Err(PersistenceError::Corruption(_))
        ));
    }
}
