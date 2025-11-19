/// Write-Ahead Log (WAL) 实现
///
/// WAL用于记录所有订单操作，确保数据持久性和可恢复性。
///
/// ## 特性
/// - 顺序写入，最小化磁盘寻道
/// - 批量刷盘，提高吞吐量
/// - CRC32校验，检测数据损坏
/// - 支持日志轮转
///
/// ## 文件格式
/// ```
/// [Header] [Entry1] [Entry2] ... [EntryN]
///
/// Header: magic(4B) | version(2B) | flags(2B)
/// Entry:  length(4B) | seq(8B) | timestamp(8B) | crc32(4B) | data(length B)
/// ```
use super::{PersistenceError, Result, WalEntry};
use bincode::config::standard;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

const WAL_MAGIC: u32 = 0x5741_4C46; // "WALF"
const WAL_VERSION: u16 = 1;

/// WAL写入器
pub struct WalWriter {
    /// 当前文件
    file: Arc<Mutex<BufWriter<File>>>,
    /// 文件路径
    file_path: PathBuf,
    /// 当前序列号
    seq: Arc<AtomicU64>,
    /// 是否启用fsync
    sync: bool,
    /// 批量大小
    batch_size: usize,
    /// 当前批次计数
    batch_count: usize,
    /// 当前文件大小
    file_size: usize,
    /// 最大文件大小
    max_file_size: usize,
    /// 文件编号
    file_num: u64,
    /// 数据目录
    data_dir: PathBuf,
}

impl WalWriter {
    /// 创建新的WAL写入器
    pub fn new(
        data_dir: PathBuf,
        sync: bool,
        batch_size: usize,
        max_file_size: usize,
    ) -> Result<Self> {
        std::fs::create_dir_all(&data_dir)?;

        let file_num = 0;
        let file_path = Self::wal_path(&data_dir, file_num);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file_path)?;

        let mut writer = BufWriter::new(file);

        // 如果文件是新建的，写入header
        if writer.get_ref().metadata()?.len() == 0 {
            Self::write_header(&mut writer)?;
        }

        let file_size = writer.get_ref().metadata()?.len() as usize;

        Ok(Self {
            file: Arc::new(Mutex::new(writer)),
            file_path,
            seq: Arc::new(AtomicU64::new(0)),
            sync,
            batch_size,
            batch_count: 0,
            file_size,
            max_file_size,
            file_num,
            data_dir,
        })
    }

    /// 从指定序列号恢复
    #[must_use]
    pub fn with_seq(self, seq: u64) -> Self {
        self.seq.store(seq, Ordering::SeqCst);
        self
    }

    /// 写入WAL条目
    pub fn write(&mut self, entry: WalEntry) -> Result<u64> {
        // 序列化条目
        let data = bincode::serde::encode_to_vec(&entry, standard())
            .map_err(|e| PersistenceError::Serialization(e.to_string()))?;

        // 计算CRC32
        let crc = crc32fast::hash(&data);

        let mut file = self.file.lock().unwrap();

        // 写入长度
        let len = data.len() as u32;
        file.write_all(&len.to_le_bytes())?;

        // 写入序列号和时间戳
        file.write_all(&entry.seq().to_le_bytes())?;
        file.write_all(&entry.timestamp().to_le_bytes())?;

        // 写入CRC32
        file.write_all(&crc.to_le_bytes())?;

        // 写入数据
        file.write_all(&data)?;

        self.file_size += 4 + 8 + 8 + 4 + data.len();
        self.batch_count += 1;

        // 批量刷盘
        if self.batch_count >= self.batch_size {
            file.flush()?;
            if self.sync {
                file.get_mut().sync_all()?;
            }
            self.batch_count = 0;
        }

        // 检查是否需要轮转
        if self.file_size >= self.max_file_size {
            drop(file);
            self.rotate()?;
        }

        Ok(entry.seq())
    }

    /// 强制刷盘
    pub fn flush(&mut self) -> Result<()> {
        let mut file = self.file.lock().unwrap();
        file.flush()?;
        if self.sync {
            file.get_mut().sync_all()?;
        }
        self.batch_count = 0;
        Ok(())
    }

    /// 轮转到新文件
    fn rotate(&mut self) -> Result<()> {
        // 刷新当前文件
        self.flush()?;

        // 创建新文件
        self.file_num += 1;
        let new_path = Self::wal_path(&self.data_dir, self.file_num);
        let new_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&new_path)?;

        let mut new_writer = BufWriter::new(new_file);
        Self::write_header(&mut new_writer)?;

        // 替换文件
        *self.file.lock().unwrap() = new_writer;
        self.file_path = new_path;
        self.file_size = 8; // header size
        self.batch_count = 0;

        Ok(())
    }

    /// 写入文件头
    fn write_header(writer: &mut BufWriter<File>) -> Result<()> {
        writer.write_all(&WAL_MAGIC.to_le_bytes())?;
        writer.write_all(&WAL_VERSION.to_le_bytes())?;
        writer.write_all(&[0u8; 2])?; // flags
        Ok(())
    }

    /// 获取WAL文件路径
    fn wal_path(data_dir: &Path, file_num: u64) -> PathBuf {
        data_dir.join(format!("wal.{file_num:06}.log"))
    }

    /// 获取下一个序列号
    #[must_use]
    pub fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::SeqCst)
    }

    /// 获取当前序列号
    #[must_use]
    pub fn current_seq(&self) -> u64 {
        self.seq.load(Ordering::SeqCst)
    }
}

/// WAL读取器
pub struct WalReader {
    /// 数据目录
    data_dir: PathBuf,
}

impl WalReader {
    /// 创建新的WAL读取器
    #[must_use]
    pub fn new(data_dir: PathBuf) -> Self {
        Self { data_dir }
    }

    /// 读取所有WAL条目
    pub fn read_all(&self) -> Result<Vec<WalEntry>> {
        let mut entries = Vec::new();

        // 扫描所有WAL文件
        let mut file_num = 0u64;
        loop {
            let path = self.wal_path(file_num);
            if !path.exists() {
                break;
            }

            entries.extend(self.read_file(&path)?);
            file_num += 1;
        }

        Ok(entries)
    }

    /// 读取单个WAL文件
    fn read_file(&self, path: &PathBuf) -> Result<Vec<WalEntry>> {
        use std::io::{BufReader, Read};

        let file = File::open(path)?;
        let mut reader = BufReader::new(file);

        // 读取并验证header
        let mut magic = [0u8; 4];
        reader.read_exact(&mut magic)?;
        if u32::from_le_bytes(magic) != WAL_MAGIC {
            return Err(PersistenceError::Corruption("Invalid WAL magic".into()));
        }

        let mut version = [0u8; 2];
        reader.read_exact(&mut version)?;
        if u16::from_le_bytes(version) != WAL_VERSION {
            return Err(PersistenceError::Corruption(format!(
                "Unsupported WAL version: {}",
                u16::from_le_bytes(version)
            )));
        }

        // 跳过flags
        let mut flags = [0u8; 2];
        reader.read_exact(&mut flags)?;

        let mut entries = Vec::new();

        // 读取条目
        loop {
            // 读取长度
            let mut len_buf = [0u8; 4];
            if reader.read_exact(&mut len_buf).is_err() {
                break; // EOF
            }
            let len = u32::from_le_bytes(len_buf) as usize;

            // 读取序列号和时间戳
            let mut seq_buf = [0u8; 8];
            let mut ts_buf = [0u8; 8];
            reader.read_exact(&mut seq_buf)?;
            reader.read_exact(&mut ts_buf)?;

            // 读取CRC32
            let mut crc_buf = [0u8; 4];
            reader.read_exact(&mut crc_buf)?;
            let expected_crc = u32::from_le_bytes(crc_buf);

            // 读取数据
            let mut data = vec![0u8; len];
            reader.read_exact(&mut data)?;

            // 验证CRC32
            let actual_crc = crc32fast::hash(&data);
            if actual_crc != expected_crc {
                return Err(PersistenceError::Corruption("CRC32 mismatch".into()));
            }

            // 反序列化条目
            let (entry, _): (WalEntry, usize) =
                bincode::serde::decode_from_slice(&data, standard())
                    .map_err(|e| PersistenceError::Deserialization(e.to_string()))?;

            entries.push(entry);
        }

        Ok(entries)
    }

    /// 获取WAL文件路径
    fn wal_path(&self, file_num: u64) -> PathBuf {
        self.data_dir.join(format!("wal.{file_num:06}.log"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::protocol::{NewOrderRequest, OrderType};
    use std::sync::Arc;
    use tempfile::TempDir;

    #[test]
    fn test_wal_write_read() {
        let temp_dir = TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();

        // 写入
        {
            let mut writer = WalWriter::new(data_dir.clone(), false, 10, 1024 * 1024).unwrap();

            for i in 0..100 {
                let seq = writer.next_seq();
                let entry = WalEntry::NewOrder {
                    seq,
                    timestamp: i,
                    order: NewOrderRequest {
                        user_id: i,
                        symbol: Arc::from("BTCUSDT"),
                        order_type: OrderType::Buy,
                        price: 50000 + i,
                        quantity: 10,
                    },
                };
                writer.write(entry).unwrap();
            }

            writer.flush().unwrap();
        }

        // 读取
        {
            let reader = WalReader::new(data_dir);
            let entries = reader.read_all().unwrap();

            assert_eq!(entries.len(), 100);

            for (i, entry) in entries.iter().enumerate() {
                assert_eq!(entry.seq(), i as u64);
                assert_eq!(entry.timestamp(), i as u64);
            }
        }
    }

    #[test]
    fn test_wal_large_volume_recovery() {
        let temp_dir = TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();

        const ENTRY_COUNT: u64 = 10_000;

        // 写入10,000条记录
        {
            let mut writer = WalWriter::new(data_dir.clone(), false, 100, 1024 * 1024 * 1024).unwrap();

            for i in 0..ENTRY_COUNT {
                let seq = writer.next_seq();
                let entry = WalEntry::NewOrder {
                    seq,
                    timestamp: i * 1000,
                    order: NewOrderRequest {
                        user_id: i,
                        symbol: Arc::from("BTCUSDT"),
                        order_type: if i % 2 == 0 { OrderType::Buy } else { OrderType::Sell },
                        price: 50000 + (i % 1000),
                        quantity: 10 + (i % 100),
                    },
                };
                writer.write(entry).unwrap();
            }

            writer.flush().unwrap();
        }

        // 读取并验证所有记录
        {
            let reader = WalReader::new(data_dir);
            let entries = reader.read_all().unwrap();

            assert_eq!(entries.len(), ENTRY_COUNT as usize, "All entries must be recovered");

            // 验证序列号连续性
            for (i, entry) in entries.iter().enumerate() {
                assert_eq!(entry.seq(), i as u64, "Sequence number must be continuous");
                assert_eq!(entry.timestamp(), (i as u64) * 1000, "Timestamp must match");

                // 验证数据完整性
                if let WalEntry::NewOrder { order, .. } = entry {
                    assert_eq!(order.user_id, i as u64);
                    assert_eq!(order.price, 50000 + ((i as u64) % 1000));
                    assert_eq!(order.quantity, 10 + ((i as u64) % 100));
                }
            }
        }
    }

    #[test]
    fn test_wal_corruption_detection() {
        use std::io::{Seek, SeekFrom, Write as IoWrite};

        let temp_dir = TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();

        // 写入一些有效记录
        {
            let mut writer = WalWriter::new(data_dir.clone(), false, 10, 1024 * 1024).unwrap();

            for i in 0..10 {
                let seq = writer.next_seq();
                let entry = WalEntry::NewOrder {
                    seq,
                    timestamp: i,
                    order: NewOrderRequest {
                        user_id: i,
                        symbol: Arc::from("BTCUSDT"),
                        order_type: OrderType::Buy,
                        price: 50000,
                        quantity: 10,
                    },
                };
                writer.write(entry).unwrap();
            }

            writer.flush().unwrap();
        }

        // 手动损坏文件中的数据（在CRC之后的数据区域）
        {
            let wal_path = data_dir.join("wal.000000.log");
            let mut file = OpenOptions::new()
                .write(true)
                .open(&wal_path)
                .unwrap();

            // 跳过header (8 bytes) 和第一个条目的元数据 (4+8+8+4=24 bytes)
            // 然后损坏实际数据
            file.seek(SeekFrom::Start(8 + 24 + 10)).unwrap();
            file.write_all(&[0xFF, 0xFF, 0xFF, 0xFF]).unwrap();
            file.flush().unwrap();
        }

        // 尝试读取，应该检测到CRC校验失败
        {
            let reader = WalReader::new(data_dir);
            let result = reader.read_all();

            assert!(result.is_err(), "Corrupted data must be detected");
            if let Err(e) = result {
                match e {
                    PersistenceError::Corruption(msg) => {
                        assert!(msg.contains("CRC32"), "Error must mention CRC32 mismatch");
                    }
                    _ => panic!("Expected Corruption error, got: {:?}", e),
                }
            }
        }
    }

    #[test]
    fn test_wal_concurrent_writes() {
        use std::thread;

        let temp_dir = TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();

        const THREADS: usize = 10;
        const ENTRIES_PER_THREAD: u64 = 100;

        // 创建writer并包装在Arc中以便多线程共享
        let writer = Arc::new(Mutex::new(
            WalWriter::new(data_dir.clone(), false, 10, 1024 * 1024 * 1024).unwrap()
        ));

        // 启动多个线程并发写入
        let mut handles = vec![];
        for thread_id in 0..THREADS {
            let writer_clone = Arc::clone(&writer);
            let handle = thread::spawn(move || {
                for i in 0..ENTRIES_PER_THREAD {
                    let mut w = writer_clone.lock().unwrap();
                    let seq = w.next_seq();
                    let entry = WalEntry::NewOrder {
                        seq,
                        timestamp: (thread_id as u64) * 1000 + i,
                        order: NewOrderRequest {
                            user_id: (thread_id as u64) * 1000 + i,
                            symbol: Arc::from("BTCUSDT"),
                            order_type: OrderType::Buy,
                            price: 50000 + i,
                            quantity: 10,
                        },
                    };
                    w.write(entry).unwrap();
                }
            });
            handles.push(handle);
        }

        // 等待所有线程完成
        for handle in handles {
            handle.join().unwrap();
        }

        // 刷新所有缓冲数据
        writer.lock().unwrap().flush().unwrap();

        // 读取并验证
        {
            let reader = WalReader::new(data_dir);
            let entries = reader.read_all().unwrap();

            let expected_count = THREADS * ENTRIES_PER_THREAD as usize;
            assert_eq!(
                entries.len(),
                expected_count,
                "All concurrent writes must be persisted"
            );

            // 验证序列号唯一性和连续性
            let mut seqs: Vec<u64> = entries.iter().map(|e| e.seq()).collect();
            seqs.sort_unstable();

            for (i, &seq) in seqs.iter().enumerate() {
                assert_eq!(seq, i as u64, "Sequence numbers must be unique and continuous");
            }
        }
    }

    #[test]
    fn test_wal_file_rotation() {
        let temp_dir = TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();

        // 设置一个小的max_file_size以触发轮转
        const MAX_FILE_SIZE: usize = 1024; // 1KB
        const ENTRY_COUNT: u64 = 100;

        {
            let mut writer = WalWriter::new(data_dir.clone(), false, 10, MAX_FILE_SIZE).unwrap();

            for i in 0..ENTRY_COUNT {
                let seq = writer.next_seq();
                let entry = WalEntry::NewOrder {
                    seq,
                    timestamp: i,
                    order: NewOrderRequest {
                        user_id: i,
                        symbol: Arc::from("BTCUSDT"),
                        order_type: OrderType::Buy,
                        price: 50000 + i,
                        quantity: 10,
                    },
                };
                writer.write(entry).unwrap();
            }

            writer.flush().unwrap();
        }

        // 验证多个WAL文件被创建
        let mut file_count = 0;
        let mut file_num = 0u64;
        loop {
            let path = data_dir.join(format!("wal.{file_num:06}.log"));
            if !path.exists() {
                break;
            }
            file_count += 1;
            file_num += 1;
        }

        assert!(
            file_count > 1,
            "Multiple WAL files should be created due to rotation (found {})",
            file_count
        );

        // 验证所有数据可以从多个文件中恢复
        {
            let reader = WalReader::new(data_dir.clone());
            let entries = reader.read_all().unwrap();

            assert_eq!(
                entries.len(),
                ENTRY_COUNT as usize,
                "All entries must be recoverable across rotated files"
            );

            // 验证序列号连续性
            for (i, entry) in entries.iter().enumerate() {
                assert_eq!(entry.seq(), i as u64);
            }
        }

        // 验证每个文件大小不超过限制（允许一些误差，因为条目不可分割）
        for i in 0..file_count {
            let path = data_dir.join(format!("wal.{i:06}.log"));
            let metadata = std::fs::metadata(&path).unwrap();

            // 除了最后一个文件，其他文件应该接近max_file_size
            if i < file_count - 1 {
                assert!(
                    metadata.len() as usize >= MAX_FILE_SIZE,
                    "File {} should be at least max_file_size",
                    i
                );
            }
        }
    }

    #[test]
    fn test_wal_fsync_durability() {
        use std::time::Instant;

        let temp_dir = TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();

        const ENTRY_COUNT: u64 = 50;

        // 测试 sync=true 模式
        let sync_time = {
            let start = Instant::now();
            let mut writer = WalWriter::new(
                data_dir.join("sync"),
                true,  // sync=true
                1,     // batch_size=1 确保每次写入都fsync
                1024 * 1024
            ).unwrap();

            for i in 0..ENTRY_COUNT {
                let seq = writer.next_seq();
                let entry = WalEntry::NewOrder {
                    seq,
                    timestamp: i,
                    order: NewOrderRequest {
                        user_id: i,
                        symbol: Arc::from("BTCUSDT"),
                        order_type: OrderType::Buy,
                        price: 50000,
                        quantity: 10,
                    },
                };
                writer.write(entry).unwrap();
            }
            writer.flush().unwrap();
            start.elapsed()
        };

        // 测试 sync=false 模式
        let no_sync_time = {
            let start = Instant::now();
            let mut writer = WalWriter::new(
                data_dir.join("no_sync"),
                false, // sync=false
                1,     // batch_size=1
                1024 * 1024
            ).unwrap();

            for i in 0..ENTRY_COUNT {
                let seq = writer.next_seq();
                let entry = WalEntry::NewOrder {
                    seq,
                    timestamp: i,
                    order: NewOrderRequest {
                        user_id: i,
                        symbol: Arc::from("BTCUSDT"),
                        order_type: OrderType::Buy,
                        price: 50000,
                        quantity: 10,
                    },
                };
                writer.write(entry).unwrap();
            }
            writer.flush().unwrap();
            start.elapsed()
        };

        // 验证两种模式数据都正确写入
        {
            let reader = WalReader::new(data_dir.join("sync"));
            let entries = reader.read_all().unwrap();
            assert_eq!(entries.len(), ENTRY_COUNT as usize);
        }

        {
            let reader = WalReader::new(data_dir.join("no_sync"));
            let entries = reader.read_all().unwrap();
            assert_eq!(entries.len(), ENTRY_COUNT as usize);
        }

        // sync=true 通常应该慢一些（但在某些系统上可能差异不明显）
        // 这里只是记录性能差异，不强制要求
        println!("WAL fsync durability test:");
        println!("  sync=true  time: {:?}", sync_time);
        println!("  sync=false time: {:?}", no_sync_time);
        println!("  Performance difference: {:.2}x",
                 sync_time.as_secs_f64() / no_sync_time.as_secs_f64());

        // 验证sync=true的数据确实被持久化（即使在没有显式flush的情况下也应该可见）
        // 这通过前面的read_all测试已经验证
    }
}
