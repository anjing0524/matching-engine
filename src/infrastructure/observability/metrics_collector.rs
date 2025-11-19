use parking_lot::RwLock;
/// 指标收集器
///
/// 收集系统运行时指标，用于健康检查和监控
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// 全局指标收集器
pub struct MetricsCollector {
    /// 活跃连接数
    active_connections: AtomicUsize,
    /// 延迟样本（微秒）
    latency_samples: Arc<RwLock<LatencyTracker>>,
}

/// 延迟跟踪器
struct LatencyTracker {
    samples: Vec<u64>,
    capacity: usize,
    total: u64,
    count: u64,
}

impl LatencyTracker {
    fn new(capacity: usize) -> Self {
        Self {
            samples: Vec::with_capacity(capacity),
            capacity,
            total: 0,
            count: 0,
        }
    }

    fn add_sample(&mut self, latency_us: u64) {
        self.total += latency_us;
        self.count += 1;

        if self.samples.len() < self.capacity {
            self.samples.push(latency_us);
        } else {
            // 滑动窗口：替换最旧的样本
            let idx = (self.count as usize - 1) % self.capacity;
            self.samples[idx] = latency_us;
        }
    }

    fn average(&self) -> f64 {
        if self.count == 0 {
            return 0.0;
        }

        // 使用最近的样本计算平均值
        if self.samples.is_empty() {
            return 0.0;
        }

        let sum: u64 = self.samples.iter().sum();
        sum as f64 / self.samples.len() as f64
    }

    fn p99(&self) -> u64 {
        if self.samples.is_empty() {
            return 0;
        }

        let mut sorted = self.samples.clone();
        sorted.sort_unstable();

        let idx = (sorted.len() as f64 * 0.99) as usize;
        sorted[idx.min(sorted.len() - 1)]
    }
}

impl MetricsCollector {
    /// 创建新的指标收集器
    #[must_use]
    pub fn new() -> Self {
        Self {
            active_connections: AtomicUsize::new(0),
            latency_samples: Arc::new(RwLock::new(LatencyTracker::new(10_000))),
        }
    }

    /// 增加连接计数
    pub fn inc_connections(&self) {
        self.active_connections.fetch_add(1, Ordering::Relaxed);
    }

    /// 减少连接计数
    pub fn dec_connections(&self) {
        self.active_connections.fetch_sub(1, Ordering::Relaxed);
    }

    /// 获取活跃连接数
    pub fn active_connections(&self) -> usize {
        self.active_connections.load(Ordering::Relaxed)
    }

    /// 记录延迟样本
    pub fn record_latency(&self, latency_us: u64) {
        let mut tracker = self.latency_samples.write();
        tracker.add_sample(latency_us);
    }

    /// 获取平均延迟（微秒）
    pub fn avg_latency_us(&self) -> f64 {
        let tracker = self.latency_samples.read();
        tracker.average()
    }

    /// 获取P99延迟（微秒）
    pub fn p99_latency_us(&self) -> u64 {
        let tracker = self.latency_samples.read();
        tracker.p99()
    }

    /// 获取内存使用率
    pub fn memory_usage_percent(&self) -> f64 {
        // 尝试读取/proc/self/status (Linux)
        #[cfg(target_os = "linux")]
        {
            use std::fs;

            if let Ok(status) = fs::read_to_string("/proc/self/status") {
                let mut vm_rss = 0u64;
                let mut vm_size = 0u64;

                for line in status.lines() {
                    if line.starts_with("VmRSS:") {
                        if let Some(value) = line.split_whitespace().nth(1) {
                            vm_rss = value.parse().unwrap_or(0);
                        }
                    } else if line.starts_with("VmSize:") {
                        if let Some(value) = line.split_whitespace().nth(1) {
                            vm_size = value.parse().unwrap_or(0);
                        }
                    }
                }

                if vm_size > 0 {
                    return (vm_rss as f64 / vm_size as f64) * 100.0;
                }
            }
        }

        // 如果无法读取，返回默认值
        // TODO: 实现 jemalloc 统计API获取内存使用率

        // 默认返回0（未知）
        0.0
    }
}

impl Default for MetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}

lazy_static::lazy_static! {
    /// 全局指标收集器实例
    pub static ref GLOBAL_METRICS: Arc<MetricsCollector> = Arc::new(MetricsCollector::new());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_collector() {
        let collector = MetricsCollector::new();

        // 测试连接计数
        assert_eq!(collector.active_connections(), 0);
        collector.inc_connections();
        assert_eq!(collector.active_connections(), 1);
        collector.dec_connections();
        assert_eq!(collector.active_connections(), 0);
    }

    #[test]
    fn test_latency_tracking() {
        let collector = MetricsCollector::new();

        // 记录一些延迟样本
        collector.record_latency(100);
        collector.record_latency(200);
        collector.record_latency(300);

        let avg = collector.avg_latency_us();
        assert!((avg - 200.0).abs() < 0.1, "Average should be ~200");

        let p99 = collector.p99_latency_us();
        assert_eq!(p99, 300, "P99 should be 300");
    }

    #[test]
    fn test_latency_tracker_window() {
        let mut tracker = LatencyTracker::new(3);

        // 添加超过容量的样本
        tracker.add_sample(100);
        tracker.add_sample(200);
        tracker.add_sample(300);
        tracker.add_sample(400); // 应该替换100

        assert_eq!(tracker.samples.len(), 3);
        let avg = tracker.average();
        // 平均值应该是最近3个样本
        assert!((avg - 300.0).abs() < 0.1);
    }
}
