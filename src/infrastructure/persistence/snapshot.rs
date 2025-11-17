/// 快照（Snapshot）实现
///
/// 定期保存完整的订单簿状态，加速恢复过程。
///
/// ## 特性
/// - 压缩快照数据（可选）
/// - 校验和验证
/// - 原子性写入（先写临时文件，然后重命名）
/// - 自动清理旧快照

use super::{PersistenceError, Result, SnapshotData};
use crate::infrastructure::persistence::types::SnapshotMetadata;
use bincode::config::standard;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// 快照写入器
pub struct SnapshotWriter {
    /// 数据目录
    data_dir: PathBuf,
    /// 是否启用压缩
    compression: bool,
    /// 保留的快照数量
    retention: usize,
}

impl SnapshotWriter {
    /// 创建新的快照写入器
    pub fn new(data_dir: PathBuf, compression: bool, retention: usize) -> Result<Self> {
        fs::create_dir_all(&data_dir)?;
        Ok(Self {
            data_dir,
            compression,
            retention,
        })
    }

    /// 写入快照
    pub fn write(&self, snapshot: SnapshotData) -> Result<u64> {
        let snapshot_id = snapshot.snapshot_id;

        // 临时文件路径
        let temp_path = self.snapshot_path(snapshot_id).with_extension("tmp");
        let final_path = self.snapshot_path(snapshot_id);

        // 序列化快照
        let mut data = bincode::serde::encode_to_vec(&snapshot, standard())
            .map_err(|e| PersistenceError::Serialization(e.to_string()))?;

        // 可选压缩
        if self.compression {
            data = Self::compress(&data)?;
        }

        // 写入临时文件
        {
            let file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&temp_path)?;

            let mut writer = BufWriter::new(file);
            writer.write_all(&data)?;
            writer.flush()?;
            writer.get_mut().sync_all()?;
        }

        // 原子性重命名
        fs::rename(&temp_path, &final_path)?;

        // 写入元数据
        self.write_metadata(snapshot_id, &snapshot)?;

        // 清理旧快照
        self.cleanup_old_snapshots()?;

        Ok(snapshot_id)
    }

    /// 写入元数据
    fn write_metadata(&self, snapshot_id: u64, snapshot: &SnapshotData) -> Result<()> {
        let metadata = SnapshotMetadata {
            snapshot_id,
            wal_seq: snapshot.wal_seq,
            created_at: snapshot.timestamp,
            file_size: snapshot.orderbook_state.len() as u64,
            checksum: snapshot.checksum,
        };

        let meta_path = self.metadata_path(snapshot_id);
        let data = bincode::serde::encode_to_vec(&metadata, standard())
            .map_err(|e| PersistenceError::Serialization(e.to_string()))?;

        let mut file = File::create(meta_path)?;
        file.write_all(&data)?;
        file.sync_all()?;

        Ok(())
    }

    /// 清理旧快照
    fn cleanup_old_snapshots(&self) -> Result<()> {
        let mut snapshots = self.list_snapshots()?;
        if snapshots.len() <= self.retention {
            return Ok(());
        }

        // 按时间排序
        snapshots.sort_by_key(|m| m.created_at);

        // 删除最旧的快照
        let to_delete = snapshots.len() - self.retention;
        for metadata in snapshots.iter().take(to_delete) {
            let snapshot_path = self.snapshot_path(metadata.snapshot_id);
            let meta_path = self.metadata_path(metadata.snapshot_id);

            let _ = fs::remove_file(snapshot_path);
            let _ = fs::remove_file(meta_path);
        }

        Ok(())
    }

    /// 列出所有快照
    fn list_snapshots(&self) -> Result<Vec<SnapshotMetadata>> {
        let mut snapshots = Vec::new();

        for entry in fs::read_dir(&self.data_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().and_then(|s| s.to_str()) == Some("snap.meta") {
                if let Ok(metadata) = self.read_metadata_file(&path) {
                    snapshots.push(metadata);
                }
            }
        }

        Ok(snapshots)
    }

    /// 读取元数据文件
    fn read_metadata_file(&self, path: &PathBuf) -> Result<SnapshotMetadata> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut data = Vec::new();
        reader.read_to_end(&mut data)?;

        bincode::serde::decode_from_slice(&data, standard())
            .map(|(meta, _)| meta)
            .map_err(|e| PersistenceError::Deserialization(e.to_string()))
    }

    /// 压缩数据
    fn compress(data: &[u8]) -> Result<Vec<u8>> {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data)?;
        Ok(encoder.finish()?)
    }

    /// 获取快照文件路径
    fn snapshot_path(&self, snapshot_id: u64) -> PathBuf {
        self.data_dir.join(format!("snapshot.{}.snap", snapshot_id))
    }

    /// 获取元数据文件路径
    fn metadata_path(&self, snapshot_id: u64) -> PathBuf {
        self.data_dir.join(format!("snapshot.{}.snap.meta", snapshot_id))
    }
}

/// 快照读取器
pub struct SnapshotReader {
    /// 数据目录
    data_dir: PathBuf,
}

impl SnapshotReader {
    /// 创建新的快照读取器
    pub fn new(data_dir: PathBuf) -> Self {
        Self { data_dir }
    }

    /// 读取最新快照
    pub fn read_latest(&self) -> Result<Option<SnapshotData>> {
        let metadata = self.find_latest_metadata()?;

        match metadata {
            Some(meta) => {
                let snapshot = self.read(meta.snapshot_id)?;
                Ok(Some(snapshot))
            }
            None => Ok(None),
        }
    }

    /// 读取指定快照
    pub fn read(&self, snapshot_id: u64) -> Result<SnapshotData> {
        let path = self.snapshot_path(snapshot_id);

        if !path.exists() {
            return Err(PersistenceError::FileNotFound(path));
        }

        // 读取文件
        let file = File::open(&path)?;
        let mut reader = BufReader::new(file);
        let mut data = Vec::new();
        reader.read_to_end(&mut data)?;

        // 尝试解压缩
        if Self::is_compressed(&data) {
            data = Self::decompress(&data)?;
        }

        // 反序列化
        let (snapshot, _): (SnapshotData, usize) = bincode::serde::decode_from_slice(&data, standard())
            .map_err(|e| PersistenceError::Deserialization(e.to_string()))?;

        // 验证校验和
        let actual_checksum = self.calculate_checksum(&snapshot.orderbook_state);
        if actual_checksum != snapshot.checksum {
            return Err(PersistenceError::Corruption(format!(
                "Snapshot checksum mismatch: expected {}, got {}",
                snapshot.checksum, actual_checksum
            )));
        }

        Ok(snapshot)
    }

    /// 查找最新的快照元数据
    fn find_latest_metadata(&self) -> Result<Option<SnapshotMetadata>> {
        let mut latest: Option<SnapshotMetadata> = None;

        for entry in fs::read_dir(&self.data_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().and_then(|s| s.to_str()) == Some("snap.meta") {
                if let Ok(metadata) = self.read_metadata_file(&path) {
                    match &latest {
                        None => latest = Some(metadata),
                        Some(current) if metadata.created_at > current.created_at => {
                            latest = Some(metadata)
                        }
                        _ => {}
                    }
                }
            }
        }

        Ok(latest)
    }

    /// 读取元数据文件
    fn read_metadata_file(&self, path: &PathBuf) -> Result<SnapshotMetadata> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut data = Vec::new();
        reader.read_to_end(&mut data)?;

        bincode::serde::decode_from_slice(&data, standard())
            .map(|(meta, _)| meta)
            .map_err(|e| PersistenceError::Deserialization(e.to_string()))
    }

    /// 检查数据是否被压缩
    fn is_compressed(data: &[u8]) -> bool {
        data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b
    }

    /// 解压缩数据
    fn decompress(data: &[u8]) -> Result<Vec<u8>> {
        use flate2::read::GzDecoder;
        use std::io::Read;

        let mut decoder = GzDecoder::new(data);
        let mut decompressed = Vec::new();
        decoder.read_to_end(&mut decompressed)?;
        Ok(decompressed)
    }

    /// 计算校验和
    fn calculate_checksum(&self, data: &[u8]) -> u64 {
        crc32fast::hash(data) as u64
    }

    /// 获取快照文件路径
    fn snapshot_path(&self, snapshot_id: u64) -> PathBuf {
        self.data_dir.join(format!("snapshot.{}.snap", snapshot_id))
    }
}

/// 创建快照数据
pub fn create_snapshot(wal_seq: u64, orderbook_state: Vec<u8>) -> SnapshotData {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;

    let checksum = crc32fast::hash(&orderbook_state) as u64;

    SnapshotData {
        snapshot_id: timestamp,
        wal_seq,
        timestamp,
        orderbook_state,
        checksum,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_snapshot_write_read() {
        let temp_dir = TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();

        // 创建快照
        let orderbook_state = b"test orderbook state".to_vec();
        let snapshot = create_snapshot(100, orderbook_state.clone());

        // 写入
        let writer = SnapshotWriter::new(data_dir.clone(), false, 3).unwrap();
        writer.write(snapshot.clone()).unwrap();

        // 读取
        let reader = SnapshotReader::new(data_dir);
        let loaded = reader.read_latest().unwrap().unwrap();

        assert_eq!(loaded.wal_seq, snapshot.wal_seq);
        assert_eq!(loaded.orderbook_state, orderbook_state);
        assert_eq!(loaded.checksum, snapshot.checksum);
    }

    #[test]
    fn test_snapshot_compression() {
        let temp_dir = TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();

        // 创建大数据快照
        let orderbook_state = vec![42u8; 10000];
        let snapshot = create_snapshot(100, orderbook_state.clone());

        // 写入（启用压缩）
        let writer = SnapshotWriter::new(data_dir.clone(), true, 3).unwrap();
        writer.write(snapshot.clone()).unwrap();

        // 读取
        let reader = SnapshotReader::new(data_dir);
        let loaded = reader.read_latest().unwrap().unwrap();

        assert_eq!(loaded.orderbook_state, orderbook_state);
    }
}
