//! WAL shipping frames for a warm standby.
//!
//! The primary journaler sends already-committed `WalRecord`s over a Unix
//! socket. The standby appends them to its own WAL; on failover an engine opens
//! the standby data directory and replays it.
use super::{PersistenceError, Result, WalRecord};
use crate::shared::wire::{decode, encode_to_vec};
use std::io::{self, Read, Write};

const MAX_BATCH_BYTES: usize = 64 * 1024 * 1024;

/// A batch of committed WAL records shipped to a standby.
#[derive(Debug, Clone)]
pub struct ReplicationBatch {
    pub records: Vec<WalRecord>,
}

/// One frame on the primary→standby replication stream.
#[derive(Debug, Clone)]
pub enum ReplicationFrame {
    /// Committed records after a prior bootstrap/live point.
    Live(ReplicationBatch),
    /// Snapshot file bytes plus WAL tail for a late-joining standby.
    Bootstrap {
        snapshot: Vec<u8>,
        records: Vec<WalRecord>,
    },
}

pub fn encode_batch(batch: &ReplicationBatch) -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&(batch.records.len() as u32).to_be_bytes());
    for record in &batch.records {
        payload.extend_from_slice(&record.seq.to_be_bytes());
        payload.extend_from_slice(&record.timestamp.to_be_bytes());
        let entry = encode_to_vec(&record.entry)
            .map_err(|error| PersistenceError::Serialization(error.to_string()))?;
        payload.extend_from_slice(&(entry.len() as u32).to_be_bytes());
        payload.extend_from_slice(&entry);
    }
    let mut out = Vec::with_capacity(5 + payload.len());
    out.extend_from_slice(&((1 + payload.len()) as u32).to_be_bytes());
    out.push(0);
    out.extend_from_slice(&payload);
    Ok(out)
}

pub fn encode_bootstrap(snapshot: &[u8], records: &[WalRecord]) -> Result<Vec<u8>> {
    let mut body = vec![1u8];
    body.extend_from_slice(&(snapshot.len() as u32).to_be_bytes());
    body.extend_from_slice(snapshot);
    body.extend_from_slice(&(records.len() as u32).to_be_bytes());
    for record in records {
        body.extend_from_slice(&record.seq.to_be_bytes());
        body.extend_from_slice(&record.timestamp.to_be_bytes());
        let entry = encode_to_vec(&record.entry)
            .map_err(|error| PersistenceError::Serialization(error.to_string()))?;
        body.extend_from_slice(&(entry.len() as u32).to_be_bytes());
        body.extend_from_slice(&entry);
    }
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

pub fn decode_batch(body: &[u8]) -> Result<ReplicationBatch> {
    if body.len() < 4 {
        return Err(PersistenceError::Corruption(
            "replication batch is truncated".to_string(),
        ));
    }
    let count = u32::from_be_bytes(body[..4].try_into().expect("4 bytes")) as usize;
    let mut at = 4;
    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        let header = body.get(at..at + 20).ok_or_else(|| {
            PersistenceError::Corruption("replication record header is truncated".to_string())
        })?;
        let seq = u64::from_be_bytes(header[..8].try_into().expect("8 bytes"));
        let timestamp = u64::from_be_bytes(header[8..16].try_into().expect("8 bytes"));
        let entry_len = u32::from_be_bytes(header[16..20].try_into().expect("4 bytes")) as usize;
        let start = at + 20;
        let end = start.checked_add(entry_len).ok_or_else(|| {
            PersistenceError::Corruption("replication entry length overflow".to_string())
        })?;
        let entry_bytes = body.get(start..end).ok_or_else(|| {
            PersistenceError::Corruption("replication entry is truncated".to_string())
        })?;
        let entry = decode(entry_bytes)
            .map_err(|error| PersistenceError::Deserialization(error.to_string()))?;
        records.push(WalRecord {
            seq,
            timestamp,
            entry,
        });
        at = end;
    }
    if at != body.len() {
        return Err(PersistenceError::Corruption(
            "trailing bytes in replication batch".to_string(),
        ));
    }
    Ok(ReplicationBatch { records })
}

pub fn decode_frame(body: &[u8]) -> Result<ReplicationFrame> {
    let Some((&tag, rest)) = body.split_first() else {
        return Err(PersistenceError::Corruption(
            "replication frame is truncated".to_string(),
        ));
    };
    match tag {
        0 => Ok(ReplicationFrame::Live(decode_batch(rest)?)),
        1 => {
            if rest.len() < 4 {
                return Err(PersistenceError::Corruption(
                    "replication bootstrap is truncated".to_string(),
                ));
            }
            let snapshot_len = u32::from_be_bytes(rest[..4].try_into().expect("4 bytes")) as usize;
            let snapshot_start: usize = 4;
            let snapshot_end = snapshot_start.checked_add(snapshot_len).ok_or_else(|| {
                PersistenceError::Corruption("replication snapshot length overflow".to_string())
            })?;
            let snapshot = rest
                .get(snapshot_start..snapshot_end)
                .ok_or_else(|| {
                    PersistenceError::Corruption("replication snapshot is truncated".to_string())
                })?
                .to_vec();
            let records = decode_batch(rest.get(snapshot_end..).ok_or_else(|| {
                PersistenceError::Corruption(
                    "replication bootstrap records are truncated".to_string(),
                )
            })?)?;
            Ok(ReplicationFrame::Bootstrap {
                snapshot,
                records: records.records,
            })
        }
        tag => Err(PersistenceError::Corruption(format!(
            "unknown replication frame tag {tag}"
        ))),
    }
}

pub fn read_frame<R: Read>(reader: &mut R) -> Result<Option<ReplicationFrame>> {
    let Some(len) = read_u32(reader)? else {
        return Ok(None);
    };
    let len = len as usize;
    if len > MAX_BATCH_BYTES {
        return Err(PersistenceError::Corruption(format!(
            "replication frame length {len} exceeds maximum"
        )));
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    Ok(Some(decode_frame(&body)?))
}

pub fn write_batch<W: Write>(writer: &mut W, batch: &ReplicationBatch) -> Result<()> {
    let encoded = encode_batch(batch)?;
    writer.write_all(&encoded)?;
    writer.flush()?;
    Ok(())
}

pub fn read_batch<R: Read>(reader: &mut R) -> Result<Option<ReplicationBatch>> {
    match read_frame(reader)? {
        Some(ReplicationFrame::Live(batch)) => Ok(Some(batch)),
        Some(ReplicationFrame::Bootstrap { .. }) => Err(PersistenceError::Corruption(
            "unexpected bootstrap frame in live stream".to_string(),
        )),
        None => Ok(None),
    }
}

fn read_u32<R: Read>(reader: &mut R) -> io::Result<Option<u32>> {
    let mut buf = [0u8; 4];
    let mut read = 0;
    while read < 4 {
        match reader.read(&mut buf[read..]) {
            Ok(0) if read == 0 => return Ok(None),
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated replication length",
                ));
            }
            Ok(n) => read += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(Some(u32::from_be_bytes(buf)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::SymbolId;
    use crate::shared::protocol::{NewOrderRequest, OrderType};

    fn batch() -> ReplicationBatch {
        ReplicationBatch {
            records: vec![WalRecord {
                seq: 7,
                timestamp: 99,
                entry: super::super::WalEntry::NewOrder(NewOrderRequest {
                    user_id: 1,
                    client_order_id: 2,
                    symbol: SymbolId::intern("BTCUSDT"),
                    order_type: OrderType::Buy,
                    price: 50_000,
                    quantity: 3,
                }),
            }],
        }
    }

    #[test]
    fn test_bootstrap_frame_roundtrip() {
        let encoded = encode_bootstrap(b"snapshot-bytes", &batch().records).unwrap();
        match decode_frame(&encoded[4..]).unwrap() {
            ReplicationFrame::Bootstrap { snapshot, records } => {
                assert_eq!(snapshot, b"snapshot-bytes");
                assert_eq!(records.len(), 1);
                assert_eq!(records[0].seq, 7);
            }
            ReplicationFrame::Live(_) => panic!("expected bootstrap frame"),
        }
    }

    #[test]
    fn test_replication_batch_roundtrip() {
        let encoded = encode_batch(&batch()).unwrap();
        let decoded = decode_frame(&encoded[4..]).unwrap();
        assert!(matches!(decoded, ReplicationFrame::Live(ref batch) if batch.records.len() == 1));
    }
}
