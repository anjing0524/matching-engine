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

当前只是 transport 原型，尚未替换 UDS：

1. 每个 gateway event loop 需要一对 request/response ring；
2. 需要 engine 侧 consumer 线程/event loop 和唤醒机制（busy-poll 或 eventfd/pipe）；
3. 需要跨进程 ring 生命周期、异常断开、ring 满背压和进程崩溃后的 fencing；
4. 需要把 ring 集成到 gateway/engine 配置和基准。

在此之前，生产路径仍使用 Unix domain socket。
