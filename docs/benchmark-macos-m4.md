# macOS Apple M4 基准

这不是生产容量承诺，只是本机 release 构建的可复现对比。测试客户端和
gateway/engine 在同一台 Mac 上运行，会争抢 CPU；没有 CPU pinning、硬件时间戳或
内核旁路。

## 环境

```text
CPU: Apple M4
logical cores: 10
performance cores: 4 physical / 4 logical
efficiency cores: 6 physical / 6 logical
OS: macOS
profile: release (lto=thin)
WAL fsync: false (data-plane throughput/latency)
gateway --io-threads: 4 (performance-core count)
```

## 方法

- **吞吐**：`load_generator`，纯 fire-and-forget，64 clients，服务器和客户端同机；
  queue capacity 100k，避免价位队列满造成拒绝。
- **延迟**：`latency_probe`，closed loop；每个 client 只保持一个 in-flight：
  挂一笔 49,000 的买单，等 Confirmation，再撤单，记录买单 RTT。
  只统计订单 RTT，不把撤单 RTT 混入。
- 单进程路径使用 legacy blocking gateway；split 路径使用 `gateway --io-threads 4`
  + engine `--engine-uds` decoupled journaler。
- 每项跑 10 秒；未做多次重复取中位数，数字会受后台负载影响。

## 结果

### Closed-loop 订单 RTT（无 fsync）

| 配置 | 吞吐 | p50 | p99 |
|---|---:|---:|---:|
| 单进程，4 clients | 35,317 orders/s | 53.5 µs | 117 µs |
| split，4 clients | 27,802 orders/s | 67.8 µs | 128 µs |
| 单进程，16 clients | 51,168 orders/s | 152 µs | 255 µs |
| split，16 clients | 56,236 orders/s | 138 µs | 211 µs |

结论：低并发时单进程少一次进程/IPC 边界，延迟更低；16 clients 并发下 split
已经能持平/略优于单进程，说明 IPC 不是不可接受的开销。

### Fire-and-forget 吞吐（无 fsync）

| 配置 | 订单速率 | p50 request | p99 request |
|---|---:|---:|---:|
| 单进程，64 clients，max-in-flight=1 | ~71,014/s | 50 µs | 1 ms |
| split，64 clients，max-in-flight=1024 | ~62,026/s | 5 ms | 100 ms |

split 的高 in-flight 吞吐接近单进程，但 request latency p50/p99 包含排队，
不能当作端到端延迟；closed-loop 数据见上表。

### fsync 模式（仅作 Mac 上磁盘代价参考）

APFS 上同步 fsync 波动很大：

| 配置 | 订单速率 | p50 request |
|---|---:|---:|
| 单进程 async fsync，16 clients | ~290/s | 100 ms |
| split decoupled journaler fsync，16 clients（group commit 前） | ~48/s | 100 ms |
| split decoupled journaler fsync，16 clients（group commit 后） | ~9.9k/s | ~10 ms |

journaler 会把已排队的多个 batch 合并成一次 commit；Mac APFS 的 fsync 仍明显限制尾延迟。

这组数字**不能**用于 Linux 生产磁盘结论；生产应使用独立 journaler + 批量 commit、
fsync 队列、或复制/持久内存方案。

## 复现

```bash
cargo build --release --bin matching-engine --bin gateway --bin load_generator --bin latency_probe

# closed-loop latency, split
target/release/matching-engine --engine-uds /tmp/e.sock --data-dir /tmp/e-data \
  --snapshot-interval 0 --wal-sync false --metrics-port 19200 --log-level error &
target/release/gateway --listen 127.0.0.1:18200 --engine-uds /tmp/e.sock \
  --io-threads 4 --max-in-flight 4 &
target/release/latency_probe --server 127.0.0.1:18200 --clients 4 --seconds 10
```

## 后续优化

- gateway→engine 从 UDS 换共享内存 SPSC；
- engine IPC writer 批量回包；
- 更大 batch 与 journaler group commit（fsync 模式）；
- CPU 隔离/绑核、硬件时间戳后再做正式尾延迟对比。
