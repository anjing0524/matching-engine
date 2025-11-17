/// 网络性能指标

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// 网络性能指标
pub struct NetworkMetrics {
    /// 接收数据包数
    pub rx_packets: AtomicU64,

    /// 发送数据包数
    pub tx_packets: AtomicU64,

    /// 接收字节数
    pub rx_bytes: AtomicU64,

    /// 发送字节数
    pub tx_bytes: AtomicU64,

    /// 丢包数
    pub dropped_packets: AtomicU64,

    /// 错误数
    pub errors: AtomicU64,

    /// 累计延迟（纳秒）
    cumulative_latency_ns: AtomicU64,

    /// 延迟样本数
    latency_samples: AtomicU64,

    /// 启动时间
    start_time: Instant,
}

impl NetworkMetrics {
    /// 创建新的指标
    pub fn new() -> Self {
        Self {
            rx_packets: AtomicU64::new(0),
            tx_packets: AtomicU64::new(0),
            rx_bytes: AtomicU64::new(0),
            tx_bytes: AtomicU64::new(0),
            dropped_packets: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            cumulative_latency_ns: AtomicU64::new(0),
            latency_samples: AtomicU64::new(0),
            start_time: Instant::now(),
        }
    }

    /// 记录接收数据包
    #[inline]
    pub fn record_rx_packet(&self, bytes: usize) {
        self.rx_packets.fetch_add(1, Ordering::Relaxed);
        self.rx_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    /// 记录发送数据包
    #[inline]
    pub fn record_tx_packet(&self, bytes: usize) {
        self.tx_packets.fetch_add(1, Ordering::Relaxed);
        self.tx_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    /// 记录丢包
    #[inline]
    pub fn record_dropped(&self) {
        self.dropped_packets.fetch_add(1, Ordering::Relaxed);
    }

    /// 记录错误
    #[inline]
    pub fn record_error(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }

    /// 记录延迟
    #[inline]
    pub fn record_latency(&self, latency: Duration) {
        self.cumulative_latency_ns
            .fetch_add(latency.as_nanos() as u64, Ordering::Relaxed);
        self.latency_samples.fetch_add(1, Ordering::Relaxed);
    }

    /// 获取指标快照
    pub fn snapshot(&self) -> MetricsSnapshot {
        let rx_packets = self.rx_packets.load(Ordering::Relaxed);
        let tx_packets = self.tx_packets.load(Ordering::Relaxed);
        let rx_bytes = self.rx_bytes.load(Ordering::Relaxed);
        let tx_bytes = self.tx_bytes.load(Ordering::Relaxed);
        let dropped = self.dropped_packets.load(Ordering::Relaxed);
        let errors = self.errors.load(Ordering::Relaxed);
        let cumulative_latency = self.cumulative_latency_ns.load(Ordering::Relaxed);
        let samples = self.latency_samples.load(Ordering::Relaxed);

        let elapsed = self.start_time.elapsed();

        MetricsSnapshot {
            rx_packets,
            tx_packets,
            rx_bytes,
            tx_bytes,
            dropped_packets: dropped,
            errors,
            avg_latency_ns: if samples > 0 {
                cumulative_latency / samples
            } else {
                0
            },
            rx_pps: (rx_packets as f64 / elapsed.as_secs_f64()) as u64,
            tx_pps: (tx_packets as f64 / elapsed.as_secs_f64()) as u64,
            rx_throughput_mbps: (rx_bytes as f64 * 8.0 / elapsed.as_secs_f64() / 1_000_000.0),
            tx_throughput_mbps: (tx_bytes as f64 * 8.0 / elapsed.as_secs_f64() / 1_000_000.0),
            drop_rate: if rx_packets > 0 {
                dropped as f64 / rx_packets as f64
            } else {
                0.0
            },
            uptime: elapsed,
        }
    }

    /// 重置指标
    pub fn reset(&self) {
        self.rx_packets.store(0, Ordering::Relaxed);
        self.tx_packets.store(0, Ordering::Relaxed);
        self.rx_bytes.store(0, Ordering::Relaxed);
        self.tx_bytes.store(0, Ordering::Relaxed);
        self.dropped_packets.store(0, Ordering::Relaxed);
        self.errors.store(0, Ordering::Relaxed);
        self.cumulative_latency_ns.store(0, Ordering::Relaxed);
        self.latency_samples.store(0, Ordering::Relaxed);
    }
}

impl Default for NetworkMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// 指标快照
#[derive(Debug, Clone)]
pub struct MetricsSnapshot {
    /// 接收数据包总数
    pub rx_packets: u64,

    /// 发送数据包总数
    pub tx_packets: u64,

    /// 接收字节总数
    pub rx_bytes: u64,

    /// 发送字节总数
    pub tx_bytes: u64,

    /// 丢包总数
    pub dropped_packets: u64,

    /// 错误总数
    pub errors: u64,

    /// 平均延迟（纳秒）
    pub avg_latency_ns: u64,

    /// 接收包速率（packets/sec）
    pub rx_pps: u64,

    /// 发送包速率（packets/sec）
    pub tx_pps: u64,

    /// 接收吞吐量（Mbps）
    pub rx_throughput_mbps: f64,

    /// 发送吞吐量（Mbps）
    pub tx_throughput_mbps: f64,

    /// 丢包率
    pub drop_rate: f64,

    /// 运行时间
    pub uptime: Duration,
}

impl MetricsSnapshot {
    /// 打印指标
    pub fn print(&self) {
        println!("=== Network Metrics ===");
        println!("Uptime: {:?}", self.uptime);
        println!("RX: {} packets ({} bytes)", self.rx_packets, self.rx_bytes);
        println!("TX: {} packets ({} bytes)", self.tx_packets, self.tx_bytes);
        println!("RX Rate: {} pps ({:.2} Mbps)", self.rx_pps, self.rx_throughput_mbps);
        println!("TX Rate: {} pps ({:.2} Mbps)", self.tx_pps, self.tx_throughput_mbps);
        println!("Dropped: {} ({:.4}%)", self.dropped_packets, self.drop_rate * 100.0);
        println!("Errors: {}", self.errors);
        println!("Avg Latency: {}µs", self.avg_latency_ns / 1000);
        println!("=======================");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_metrics() {
        let metrics = NetworkMetrics::new();

        // 记录一些数据
        metrics.record_rx_packet(1024);
        metrics.record_rx_packet(2048);
        metrics.record_tx_packet(512);
        metrics.record_dropped();
        metrics.record_latency(Duration::from_micros(100));
        metrics.record_latency(Duration::from_micros(200));

        thread::sleep(Duration::from_millis(100));

        let snapshot = metrics.snapshot();

        assert_eq!(snapshot.rx_packets, 2);
        assert_eq!(snapshot.tx_packets, 1);
        assert_eq!(snapshot.rx_bytes, 3072);
        assert_eq!(snapshot.tx_bytes, 512);
        assert_eq!(snapshot.dropped_packets, 1);
        assert_eq!(snapshot.avg_latency_ns, 150_000); // 平均150µs
    }
}
