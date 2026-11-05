# Shared-memory SPSC ring (prototype)

`src/shared/spsc_ring.rs` 实现了一个文件映射的单生产者/单消费者 ring，用于后续
gateway→engine 共享内存 IPC。

## 布局

```text
offset 0     magic u64
offset 8     version u64
offset 16    capacity u64
offset 24    slot_size u64
offset 64    head: AtomicU64   (cache-line separated)
offset 128   tail: AtomicU64   (cache-line separated)
offset 192   slots[capacity]
```

每个 slot：

```text
len: u32 | payload[slot_size] | padding to 8-byte stride
```

## 并发协议

- 单 producer 写 `head`，单 consumer 写 `tail`；
- producer 读 `tail`（Acquire）确认有空位，写 payload，最后 `head.store(Release)`；
- consumer 读 `head`（Acquire），复制 payload，最后 `tail.store(Release)`；
- slot 索引为绝对计数 `% capacity`；
- payload 超过 slot size 明确返回 `TooLarge`，满时返回 `Full`，不做隐式覆盖。

## 测试与微基准

```text
cargo test --lib spsc
cargo run --release --bin ipc_ring_bench
```

当前 Mac M4 结果：

```text
5000000 roundtrips, 20.3 ns/push+pop, 49.3 Mmsg/s
```

这是同进程双 mapping 的微基准，不是 gateway/engine 端到端延迟；端到端还会叠加
event loop、协议编解码、engine 排队和通知机制。

## 尚未集成

engine 和 gateway 侧都已支持该 transport（实验性、非默认）：

```bash
target/release/matching-engine --ipc-ring-dir ./data/ipc --ipc-ring-threads 4 \
  --data-dir ./data --snapshot-interval 0 --wal-sync false
target/release/gateway --listen 127.0.0.1:8080 --ipc-ring-dir ./data/ipc \
  --io-threads 4 --max-in-flight 64
target/release/ring_probe --ring-dir ./data/ipc --seconds 10
```

`ring_probe` 直接作为单生产者/单消费者连接 engine 的 request/response ring，测的是
engine IPC + 撮合路径，不经过 TCP/gateway。Mac M4 release 结果：

```text
ring samples=183910 throughput=18391/s
p50=23625 ns p90=41458 ns p99=90500 ns p99.9=352167 ns
```

对比更早的 UDS closed-loop（4 clients 总量）需要等 gateway ring 集成后再做同口径比较。
### 实测结论：当前 ring 路径还没有胜出

Mac M4 release，无 fsync：

| 路径 | closed-loop 4 clients | 64 clients 吞吐 |
|---|---:|---:|
| gateway UDS，4 loops | 27.8k/s, p50 67.8 µs | ~62k/s |
| gateway ring，1 loop，busy-poll | 21.4k/s, p50 87 µs | ~24k/s |
| gateway ring，4 loops，busy-poll | 18.3k/s, p50 100 µs | ~9.5k/s |
| gateway ring，4 loops，datagram notify | 26.6k/s, p50 74 µs | ~7.9k/s |
| gateway ring，4 loops，notify + ring-full backpressure | 23.2k/s, p50 79 µs | ~62k/s |

关键修正是 **ring 满时不再自旋**：gateway 暂停该 client 的读事件，保留未消费帧，
等 engine drain 后再恢复；这避免了 event-loop 卡在 `try_push` 上，吞吐从 ~8k/s
恢复到 ~62k/s，已与 UDS 高并发吞吐持平；低并发 RTT 仍略高于 UDS。

当前 ring 仍只是实验路径，默认 UDS。下一步：

1. 批量化 response 通知，减少每响应 datagram；
2. engine consumer 批处理请求；
3. 低并发 RTT 优化（减少 notify 往返）；
4. 达到/超过 UDS 后再考虑默认切换。
